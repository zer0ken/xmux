//! What xmux knows about the newest released version, and how it learns it.
//!
//! The app must not wait on GitHub to paint, and it must not ask GitHub on every
//! launch either: xmux is a session switcher, so it starts many times a day, and one
//! request per start would be a request nobody asked for. So the answer is CACHED and
//! the cache is refreshed at most once a day, off the loop.
//!
//! This is not the ssh path and the roster rule does not reach it. That rule is about
//! the machines the roster names: xmux opens no channel to one unless something asked
//! it to, because a machine that refuses a login refuses every retry identically. One
//! request a day to a release feed authenticates nothing, retries nothing, and reaches
//! no machine on the roster.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long a recorded answer stands before a launch refreshes it. A day, because a
/// release is not something a user needs to hear about within the hour, and because
/// the request costs nothing only as long as it is rare.
const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// The recorded answer: which version the release feed named, and when it was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cached {
    pub latest: String,
    pub checked_at: u64,
}

fn cache_path(xmux_dir: &Path) -> PathBuf {
    xmux_dir.join("version.json")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Renders the cache. Written by hand rather than through a serialiser because the
/// file is two fields and the version string is the only thing that varies.
pub fn render(c: &Cached) -> String {
    format!(
        "{{\"latest\":\"{}\",\"checked_at\":{}}}\n",
        c.latest.replace('"', ""),
        c.checked_at
    )
}

/// Reads the cache back. `None` for anything it cannot read as both fields, so a
/// truncated or hand-edited file is refreshed rather than believed.
pub fn parse(text: &str) -> Option<Cached> {
    let latest = field(text, "latest")?;
    let checked_at = field(text, "checked_at")?.parse().ok()?;
    if latest.is_empty() {
        return None;
    }
    Some(Cached { latest, checked_at })
}

/// The value of one JSON field, quoted or bare. The file is written by `render`, so
/// this reads that shape rather than JSON in general.
fn field(text: &str, name: &str) -> Option<String> {
    let key = format!("\"{name}\"");
    let rest = text.split_once(&key)?.1.split_once(':')?.1.trim_start();
    let value = match rest.strip_prefix('"') {
        Some(quoted) => quoted.split('"').next()?.to_string(),
        None => rest
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>(),
    };
    (!value.is_empty()).then_some(value)
}

/// The recorded answer, or `None` when nothing has been recorded yet.
pub fn read(xmux_dir: &Path) -> Option<Cached> {
    parse(&std::fs::read_to_string(cache_path(xmux_dir)).ok()?)
}

pub fn write(xmux_dir: &Path, c: &Cached) {
    let _ = std::fs::create_dir_all(xmux_dir);
    let _ = std::fs::write(cache_path(xmux_dir), render(c));
}

/// Whether an answer recorded at `checked_at` is old enough to ask again. A clock
/// that moved backwards leaves `checked_at` in the future, which reads as fresh
/// rather than as an excuse to ask on every launch.
pub fn is_stale(checked_at: u64, now: u64) -> bool {
    now.saturating_sub(checked_at) >= MAX_AGE.as_secs()
}

/// The line the app shows when a newer version has been released, or `None` when the
/// recorded answer is not newer than what is running.
///
/// It names the command rather than describing it, because the whole point of the
/// line is that the reader can act on it without looking anything up.
pub fn notice(cached: Option<&Cached>, current: &str) -> Option<String> {
    let c = cached?;
    super::release::is_newer(&c.latest, current).then(|| {
        format!(
            "xmux {} is available (running {current}) - run `xmux update`",
            c.latest
        )
    })
}

/// Refreshes the cache if the recorded answer is older than a day. Runs the request
/// on a blocking thread and returns at once, so nothing on the app's path waits for
/// GitHub; a request that fails leaves the previous answer standing, because a
/// release feed that did not answer is not news and must not become a retry.
pub fn refresh_in_background(xmux_dir: &Path, enabled: bool) {
    if !enabled {
        return;
    }
    let now = now_secs();
    if let Some(c) = read(xmux_dir) {
        if !is_stale(c.checked_at, now) {
            return;
        }
    }
    let dir = xmux_dir.to_path_buf();
    std::thread::spawn(move || {
        let Ok(latest) = super::release::latest_version() else {
            return;
        };
        write(
            &dir,
            &Cached {
                latest,
                checked_at: now_secs(),
            },
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_written_answer_reads_back_as_itself() {
        let c = Cached {
            latest: "1.2.3".into(),
            checked_at: 1_700_000_000,
        };
        assert_eq!(parse(&render(&c)), Some(c));
    }

    #[test]
    fn a_file_missing_either_field_is_refreshed_rather_than_believed() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("{\"latest\":\"1.2.3\"}"), None);
        assert_eq!(parse("{\"checked_at\":5}"), None);
        assert_eq!(parse("{\"latest\":\"\",\"checked_at\":5}"), None);
    }

    #[test]
    fn an_answer_stands_for_a_day_and_is_asked_again_after() {
        let day = MAX_AGE.as_secs();
        assert!(!is_stale(1000, 1000), "just asked");
        assert!(!is_stale(1000, 1000 + day - 1), "still inside the day");
        assert!(is_stale(1000, 1000 + day), "a day on, ask again");
    }

    #[test]
    fn a_clock_that_moved_backwards_does_not_ask_on_every_launch() {
        // `checked_at` ahead of now would otherwise subtract to a huge age and make
        // every launch a request, which is the one thing the cache exists to prevent.
        assert!(!is_stale(9_000, 1_000));
    }

    #[test]
    fn the_notice_appears_only_for_a_version_newer_than_the_running_one() {
        let at = |v: &str| Cached {
            latest: v.into(),
            checked_at: 0,
        };
        assert!(notice(Some(&at("0.9.7")), "0.9.6").is_some());
        assert_eq!(notice(Some(&at("0.9.6")), "0.9.6"), None);
        assert_eq!(notice(Some(&at("0.9.5")), "0.9.6"), None);
        assert_eq!(notice(None, "0.9.6"), None);
    }

    #[test]
    fn the_notice_names_the_command_that_acts_on_it() {
        let n = notice(
            Some(&Cached {
                latest: "1.0.0".into(),
                checked_at: 0,
            }),
            "0.9.6",
        )
        .unwrap();
        assert!(n.contains("1.0.0"), "{n}");
        assert!(n.contains("0.9.6"), "{n}");
        assert!(n.contains("xmux update"), "{n}");
    }
}
