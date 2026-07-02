//! psmux's per-machine session registry: the filesystem substrate psmux itself
//! discovers sessions with. psmux is one-server-per-session over localhost TCP
//! with no aggregate server, so this directory — not a `list-sessions` — is the
//! authoritative existence set for the local psmux mux's `enumerate`.

use std::path::{Path, PathBuf};

use crate::session::Session;

/// psmux's per-machine session registry directory (`~/.psmux`). Each live session
/// has a `<name>.port` file there (psmux is one-server-per-session over localhost
/// TCP, with this directory as its discovery substrate — there is no aggregate
/// server to list).
pub(crate) fn psmux_registry_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".psmux")
}

/// The live DEFAULT-SOCKET session names among psmux registry filenames. A live
/// session is a `<base>.port` file; sibling `.key`/`.sid`/bookkeeping files are
/// ignored. A base containing `__` is excluded: it is either the warm-pool standby
/// (`__warm__`) or a `-L`-namespaced session (`<ns>__<name>`), neither of which is
/// a default-socket session. Sorted + deduped for a stable order.
fn psmux_session_names(filenames: &[String]) -> Vec<String> {
    let mut names: Vec<String> = filenames
        .iter()
        .filter_map(|f| f.strip_suffix(".port"))
        .filter(|base| !base.contains("__"))
        .map(str::to_string)
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Reads psmux's registry `dir` and returns its live default-socket session names.
/// A missing/unreadable directory yields an empty list (no local sessions).
pub(crate) fn read_psmux_registry_dir(dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let files: Vec<String> = rd
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    psmux_session_names(&files)
}

/// Merges psmux's registry session NAMES (authoritative existence) with the
/// `list-sessions` DETAIL rows. A `list-sessions` row wins for a session it covers
/// (full windows/attached/recency); a registry name it omits is still surfaced with
/// a minimal placeholder, so a failed/partial `list-sessions` never blanks the
/// tree view. Deduped on name (a session in both sources appears once).
pub(crate) fn merge_psmux_sessions(
    source: &str,
    names: Vec<String>,
    detail: Vec<Session>,
) -> Vec<Session> {
    let covered: std::collections::HashSet<String> =
        detail.iter().map(|s| s.name.clone()).collect();
    let mut out = detail;
    for name in names {
        if !covered.contains(&name) {
            out.push(Session {
                source: source.to_string(),
                name,
                windows: 1,
                attached: false,
                last_attached: 0,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn psmux_session_names_excludes_warm_namespaced_and_non_port() {
        // psmux writes `<base>.port` per live session in its registry dir. A base
        // containing `__` is the warm-pool standby (`__warm__`) or a `-L`-namespaced
        // session (`<ns>__<name>`); neither is a default-socket session. Sibling
        // `.key`/`.sid`/bookkeeping files are not `.port` and are ignored.
        let files: Vec<String> = [
            "xmux.port",
            "build.port",
            "__warm__.port",
            "ns__sess.port",
            "xmux.key",
            "xmux.sid",
            "last_session",
            "next_session_id",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            psmux_session_names(&files),
            vec!["build".to_string(), "xmux".to_string()]
        );
    }

    #[test]
    fn psmux_session_names_empty() {
        assert!(psmux_session_names(&[]).is_empty());
    }

    #[test]
    fn read_psmux_registry_dir_scans_port_files() {
        let dir = std::env::temp_dir().join(format!("xmux-psmux-reg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in [
            "alpha.port",
            "beta.port",
            "__warm__.port",
            "x__y.port",
            "alpha.key",
        ] {
            std::fs::write(dir.join(f), b"1234").unwrap();
        }
        let got = read_psmux_registry_dir(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(got, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[test]
    fn read_psmux_registry_dir_missing_is_empty() {
        let dir = std::env::temp_dir().join("xmux-psmux-absent-zzzz-does-not-exist");
        assert!(read_psmux_registry_dir(&dir).is_empty());
    }

    #[test]
    fn merge_psmux_sessions_prefers_detail_and_keeps_registry_only() {
        // psmux's `list-sessions` aggregates every live default-socket session in one
        // call, so its rows carry the real detail (windows/attached/recency). The
        // registry (`*.port`) is the authoritative EXISTENCE set: a name present in
        // the registry but missing from the (possibly failed/partial) list-sessions
        // output is still surfaced, with minimal placeholder detail.
        let detail = vec![Session {
            source: "local".into(),
            name: "editor".into(),
            windows: 3,
            attached: true,
            last_attached: 200,
        }];
        let names = vec!["editor".to_string(), "build".to_string()];
        let got = merge_psmux_sessions("local", names, detail);
        assert_eq!(got.len(), 2, "no duplicate for the session in both sources");
        let editor = got.iter().find(|s| s.name == "editor").unwrap();
        assert_eq!(editor.windows, 3, "detail row wins (full info)");
        assert!(editor.attached);
        let build = got.iter().find(|s| s.name == "build").unwrap();
        assert_eq!(build.source, "local");
        assert_eq!(
            build.windows, 1,
            "registry-only session gets minimal placeholder detail"
        );
    }

    #[test]
    fn merge_psmux_sessions_empty_registry_falls_back_to_detail() {
        // If the registry read yields nothing (e.g. unreadable), the list-sessions
        // detail still stands on its own.
        let detail = vec![Session {
            source: "local".into(),
            name: "only".into(),
            windows: 1,
            attached: false,
            last_attached: 5,
        }];
        let got = merge_psmux_sessions("local", Vec::new(), detail);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "only");
    }

    #[test]
    fn merge_psmux_sessions_no_detail_surfaces_registry_names() {
        // The reported failure: list-sessions returns nothing even though sessions
        // exist. The registry names must still surface so the tree view is not blank.
        let got = merge_psmux_sessions("local", vec!["a".into(), "b".into()], Vec::new());
        let names: Vec<&str> = got.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
        assert!(got.iter().all(|s| s.source == "local"));
    }
}
