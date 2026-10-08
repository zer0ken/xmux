//! What xmux knows about the newest released version, and how it learns it.
//!
//! Startup records release checks here; diagnostics only read the recorded answer.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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

/// `latest` when it is newer than `current`, or `None` when it is not or is unknown.
pub fn available(latest: Option<&str>, current: &str) -> Option<String> {
    let latest = latest?;
    super::release::is_newer(latest, current).then(|| latest.to_owned())
}

/// Checks before startup, off the async runtime, and preserves the record on failure.
pub fn refresh(xmux_dir: &Path) -> Result<String, String> {
    let latest = super::release::latest_version()?;
    write(
        xmux_dir,
        &Cached {
            latest: latest.clone(),
            checked_at: now_secs(),
        },
    );
    Ok(latest)
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
    fn a_release_is_available_only_when_newer_than_the_running_one() {
        assert_eq!(available(Some("0.9.7"), "0.9.6"), Some("0.9.7".to_owned()));
        assert_eq!(available(Some("0.9.6"), "0.9.6"), None);
        assert_eq!(available(Some("0.9.5"), "0.9.6"), None);
        assert_eq!(available(None, "0.9.6"), None);
    }
}
