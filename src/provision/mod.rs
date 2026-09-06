//! Provisioning: which machines, binaries, and sources exist on this machine, and
//! the resolved runtime view over them. `config` loads the optional TOML and
//! merges it with ssh-config discovery, `roster` names the ssh targets, and
//! `discovery` probes sources concurrently; `env` resolves all of it into the
//! source list and lookups the commands share, re-resolved on every re-scan.

pub mod config;
pub mod discovery;
pub mod env;
pub mod neighbor;
// Linux and Android keep the network state behind netlink, and Android allows nothing
// else; every other OS is served by the commands in `neighbor`.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub mod netlink;
// Windows keeps the same state behind IP Helper, which is what its own cmdlets call.
#[cfg(windows)]
pub mod iphlpapi;
pub mod roster;
