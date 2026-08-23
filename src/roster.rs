//! The ROSTER: which machines xmux offers as sources.
//!
//! Separate from `machine/`, which owns how a command REACHES a machine, and from
//! `discovery`, which scans a machine for sessions. This module answers only "which
//! ssh targets exist", from one or more providers.
//!
//! Every provider yields plain ssh target names, so the rest of the app cannot tell
//! where a name came from: a tailnet peer becomes a `MachineKind::Ssh` exactly as an
//! `~/.ssh/config` alias does. That is what keeps the providers additive - adding one
//! touches this module and the config, nothing downstream.
//!
//! A provider that cannot run (the CLI is missing, the daemon is down, the output is
//! unparseable) yields an empty list rather than an error. A host source going quiet
//! must not stop xmux from offering the sources that did answer.

use std::collections::HashSet;
use std::process::Command;

/// Runs `tailscale status --json` and returns the peer aliases it reports. An absent
/// CLI, a stopped daemon, or a non-zero exit yields no aliases.
pub fn tailscale_aliases() -> Vec<String> {
    status_aliases(&tailscale_bin())
}

/// The provider itself, over a named binary. Every way the call can fail - the binary
/// does not exist, it cannot be spawned, it exits non-zero, it prints something that is
/// not the expected JSON - lands on the same empty list, so a machine without tailscale
/// simply contributes no aliases.
fn status_aliases(bin: &str) -> Vec<String> {
    match Command::new(bin).args(["status", "--json"]).output() {
        Ok(o) if o.status.success() => parse_tailscale_status(&String::from_utf8_lossy(&o.stdout)),
        _ => Vec::new(),
    }
}

/// Where the tailscale CLI lives. On Windows the installer does not put it on PATH,
/// so fall back to its fixed install location before giving up; elsewhere the bare
/// name is right and PATH resolves it.
fn tailscale_bin() -> String {
    if cfg!(windows) {
        let fixed = r"C:\Program Files\Tailscale\tailscale.exe";
        if std::path::Path::new(fixed).exists() {
            return fixed.to_string();
        }
    }
    "tailscale".to_string()
}

/// Extracts the ssh targets from `tailscale status --json` output.
///
/// The alias is the FIRST LABEL OF `DNSName`, not `HostName`: tailscale derives the
/// DNS label by lowercasing and stripping whatever the machine calls itself, so the
/// label is the name that actually resolves, and it is the name a user already has in
/// `~/.ssh/config`. `HostName` can be mixed case or non-ASCII (a machine named in
/// Korean is a real case) and would not resolve as typed.
///
/// `Self` is skipped: this machine is the `local` source, reached without ssh.
/// OFFLINE peers are skipped too. An offline peer cannot be scanned, so including it
/// would only add a row that is guaranteed to fail; a peer that comes up appears on
/// the next rescan.
pub fn parse_tailscale_status(json: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(peers) = v.get("Peer").and_then(|p| p.as_object()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    // The JSON object's iteration order is arbitrary, so sort by the resulting alias.
    // A host list that reshuffles between runs is a list the user cannot learn.
    let mut aliases: Vec<String> = Vec::new();
    for peer in peers.values() {
        if peer.get("Online").and_then(|o| o.as_bool()) != Some(true) {
            continue;
        }
        let Some(dns) = peer.get("DNSName").and_then(|d| d.as_str()) else {
            continue;
        };
        if let Some(alias) = dns_first_label(dns) {
            aliases.push(alias);
        }
    }
    aliases.sort();
    for alias in aliases {
        if seen.insert(alias.clone()) {
            out.push(alias);
        }
    }
    out
}

/// The first DNS label of a `DNSName` (`jupiter00.tail1cbccc.ts.net.` -> `jupiter00`),
/// or `None` when there is no usable label. Rejects anything that is not a plain DNS
/// label so a malformed entry cannot become an ssh argument.
fn dns_first_label(dns: &str) -> Option<String> {
    // No leading-dot tolerance: stripping it would promote the tailnet suffix to a
    // hostname (`.tail0.ts.net.` becoming `tail0`), which names no machine.
    let label = dns.trim().split('.').next()?;
    if label.is_empty() || label.len() > 63 {
        return None;
    }
    if !label
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    Some(label.to_ascii_lowercase())
}

/// Merges provider lists into one roster, preserving first-seen order and dropping
/// duplicates. Order is the caller's precedence: an `~/.ssh/config` alias comes first,
/// so a host the user has configured by hand keeps the position they gave it and a
/// provider that reports the same name adds nothing.
pub fn merge(lists: &[Vec<String>]) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for list in lists {
        for name in list {
            if seen.insert(name.clone()) {
                out.push(name.clone());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = r#"{
      "Self": { "HostName": "my-laptop", "DNSName": "my-laptop.tail0.ts.net.", "Online": true },
      "MagicDNSSuffix": "tail0.ts.net",
      "Peer": {
        "nodekey:aaa": { "HostName": "jupiter00", "DNSName": "jupiter00.tail0.ts.net.", "Online": true },
        "nodekey:bbb": { "HostName": "Kyla", "DNSName": "kyla.tail0.ts.net.", "Online": false },
        "nodekey:ccc": { "HostName": "graphai01", "DNSName": "graphai01.tail0.ts.net.", "Online": true }
      }
    }"#;

    #[test]
    fn takes_online_peers_by_their_dns_label() {
        // Sorted, so the list does not reshuffle with the JSON object's iteration order.
        assert_eq!(
            parse_tailscale_status(STATUS),
            vec!["graphai01", "jupiter00"]
        );
    }

    #[test]
    fn skips_self_and_offline_peers() {
        let got = parse_tailscale_status(STATUS);
        assert!(
            !got.contains(&"my-laptop".to_string()),
            "Self is the local source, not an ssh target: {got:?}"
        );
        assert!(
            !got.contains(&"kyla".to_string()),
            "an offline peer cannot be scanned, so it is not offered: {got:?}"
        );
    }

    #[test]
    fn the_dns_label_wins_over_hostname() {
        // A machine named in Korean still has an ASCII DNS label, and that label is
        // what resolves and what the user has in ssh config.
        let json = r#"{"Peer":{"k":{"HostName":"그래파이-이현령","DNSName":"node.tail0.ts.net.","Online":true}}}"#;
        assert_eq!(parse_tailscale_status(json), vec!["node"]);
    }

    #[test]
    fn a_provider_that_cannot_answer_yields_nothing() {
        // Not an error: one quiet provider must not stop the others being offered.
        assert!(parse_tailscale_status("").is_empty(), "empty output");
        assert!(parse_tailscale_status("not json").is_empty(), "garbage");
        assert!(parse_tailscale_status("{}").is_empty(), "no Peer key");
        assert!(
            parse_tailscale_status(r#"{"Peer":{}}"#).is_empty(),
            "no peers"
        );
    }

    #[test]
    fn a_label_that_is_not_a_dns_label_is_refused() {
        // The alias becomes an ssh argument, so anything shell-shaped is dropped
        // rather than passed along.
        for bad in [
            r#"{"Peer":{"k":{"DNSName":"a b.tail0.ts.net.","Online":true}}}"#,
            r#"{"Peer":{"k":{"DNSName":"a;rm -rf.tail0.ts.net.","Online":true}}}"#,
            r#"{"Peer":{"k":{"DNSName":".tail0.ts.net.","Online":true}}}"#,
            r#"{"Peer":{"k":{"Online":true}}}"#,
        ] {
            assert!(parse_tailscale_status(bad).is_empty(), "refused: {bad}");
        }
    }

    #[test]
    fn a_missing_cli_yields_nothing_rather_than_an_error() {
        // The provider is on by default, so a machine with no tailscale installed must
        // reach an empty list, never a spawn error that would fail the run.
        assert!(status_aliases("xmux-no-such-tailscale-binary").is_empty());
    }

    #[test]
    fn merge_keeps_first_seen_order_and_drops_duplicates() {
        let ssh = vec!["prod".to_string(), "jupiter00".to_string()];
        let ts = vec!["jupiter00".to_string(), "graphai01".to_string()];
        assert_eq!(
            merge(&[ssh, ts]),
            vec!["prod", "jupiter00", "graphai01"],
            "a hand-configured alias keeps its position; a provider repeat adds nothing"
        );
    }
}
