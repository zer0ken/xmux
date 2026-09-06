//! The ROSTER: which machines xmux offers as sources.
//!
//! Separate from `machine/`, which owns how a command REACHES a machine, and from
//! `discovery`, which scans a machine for sessions. This module answers only "which
//! ssh targets exist", from one or more providers.
//!
//! Every provider yields plain ssh target names, so nothing downstream BEHAVES
//! differently for one: a machine the OS says is next door becomes a `MachineKind::Ssh`
//! exactly as an `~/.ssh/config` alias does. That is what keeps the providers additive - adding one
//! touches this module and the config, nothing downstream.
//!
//! Which provider offered a name is kept ALONGSIDE the name, never inside it, and is
//! read for one purpose: a host that turns out unreachable names the thing that put it
//! on the roster, so the user knows which provider to look at (or turn off) rather than
//! hunting for a host they never wrote down.
//!
//! A provider that cannot run (the command is missing, the OS will not answer, the
//! output is unparseable) yields an empty list rather than an error. A host source going quiet
//! must not stop xmux from offering the sources that did answer.

use std::collections::HashSet;

/// Which provider put a host on the roster.
///
/// It is display-only: nothing branches on it, because a host reaches its machine the
/// same way whichever provider named it. The unreachable host screen shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    /// A `Host` alias in `~/.ssh/config`.
    SshConfig,
    /// A machine the OS already reaches in one hop: a peer of a tunnel this box is on,
    /// or a machine on the same link, that answers ssh.
    Neighbor,
    /// A WSL distribution `wsl.exe` listed on this machine.
    Wsl,
    /// No provider listed it: a `[[hosts]]` or `[[wsl]]` entry named it outright.
    Config,
    /// This box, which is on the roster without being offered by anything.
    Local,
}

impl Provider {
    /// What to call it on screen: the `[discovery]` key that turns it off, so the name
    /// shown is the name the user would edit. The two that no key controls say what
    /// they are instead.
    pub fn label(self) -> &'static str {
        match self {
            Provider::SshConfig => "ssh-config",
            Provider::Neighbor => "neighbors",
            Provider::Wsl => "wsl",
            Provider::Config => "config.toml",
            Provider::Local => "this box",
        }
    }
}

/// Merges provider lists into one roster, preserving first-seen order and dropping
/// duplicates, and keeping which provider each name came from. Order is the caller's
/// precedence: an `~/.ssh/config` alias comes first, so a host the user has configured
/// by hand keeps the position they gave it and a provider that reports the same name
/// adds nothing - including its attribution, since the name is already on the roster
/// and the FIRST provider is the one that put it there.
pub fn merge(lists: &[(Provider, Vec<String>)]) -> Vec<(String, Provider)> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for (provider, list) in lists {
        for name in list {
            if seen.insert(name.clone()) {
                out.push((name.clone(), *provider));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_keeps_first_seen_order_and_drops_duplicates() {
        let ssh = vec!["prod".to_string(), "jupiter00".to_string()];
        let ts = vec!["jupiter00".to_string(), "graphai01".to_string()];
        let got = merge(&[(Provider::SshConfig, ssh), (Provider::Neighbor, ts)]);
        assert_eq!(
            got.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
            vec!["prod", "jupiter00", "graphai01"],
            "a hand-configured alias keeps its position; a provider repeat adds nothing"
        );
    }

    #[test]
    fn a_repeated_name_keeps_the_provider_that_offered_it_first() {
        // The second provider adds nothing at all - not the name, and not a second
        // answer to "where did this come from".
        let got = merge(&[
            (Provider::SshConfig, vec!["jupiter00".to_string()]),
            (Provider::Neighbor, vec!["jupiter00".to_string()]),
        ]);
        assert_eq!(got, vec![("jupiter00".to_string(), Provider::SshConfig)]);
    }

    #[test]
    fn a_provider_is_labelled_by_the_key_that_turns_it_off() {
        // The label is what the user would edit, so reading it off the screen is enough
        // to act on it.
        assert_eq!(Provider::SshConfig.label(), "ssh-config");
        assert_eq!(Provider::Neighbor.label(), "neighbors");
        assert_eq!(Provider::Wsl.label(), "wsl");
    }
}
