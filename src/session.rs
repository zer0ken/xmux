//! The cross-environment data types: a [`Session`] living on a host (mux
//! server), its windows-and-panes detail, the [`Address`] pair (a host and a
//! session) that targets one across the server boundary, and the address-grammar
//! helpers that spell a host id.

/// The reserved MACHINE name for this machine. A local host id is this alone, or this
/// qualified by a mux (see [`host_id`]).
pub const LOCAL_MACHINE: &str = "local";

/// The reserved MACHINE-name namespace for a WSL distribution on this machine. A distro is
/// neither reachable over ssh nor part of this machine's own mux scope, so its machine name
/// carries the kind: `wsl.Ubuntu-24.04`. Naming the kind in the id is what lets it be
/// recovered from the id ALONE, which is what an async mux-discovery answer needs - it
/// carries a bare machine name, and the transport rebuilt from it has to be the same one
/// the launch path built.
pub const WSL_PREFIX: &str = "wsl.";

/// Separates a machine from the mux it serves inside a host id. Not `/`, which
/// already separates a host from a session name, and not a character an ssh alias or
/// a mux binary name carries.
pub const MUX_SEP: char = ':';

/// A host id: one MUX on one MACHINE. `qualified` is false when the machine serves a
/// single mux, and the id is then the bare machine alias - the spelling xmux has always
/// used, kept so a one-mux machine reads and is typed exactly as before. It is true when
/// the machine serves SEVERAL, and the id then names which of them this host is.
///
/// Whether to qualify follows the machine's CONFIGURED mux list, not what is found on
/// it, so an id does not change under the user when a mux turns out to be absent.
pub fn host_id(machine: &str, mux: &str, qualified: bool) -> String {
    if qualified {
        format!("{machine}{MUX_SEP}{mux}")
    } else {
        machine.to_string()
    }
}

/// Parts a machine from its mux wherever the pair is SHOWN. Not [`MUX_SEP`]: an id is typed
/// and a label is read, and a label is parted the way every other level of an address on
/// screen is, so one grammar covers the machine, the mux, the session and the window.
pub const MUX_LABEL_SEP: char = '/';

/// The label a machine and its mux are READ as: both halves, parted by [`MUX_LABEL_SEP`].
///
/// Always both halves. A machine serving one mux carries no mux in its ID, but it still
/// shows one, because a machine that appears with its mux on one card and without it on
/// the next reads as two different machines. The one exception is a mux nothing knows yet:
/// there is no name to put there, and the surface says so its own way (a card turns a
/// spinner in the mux's place).
pub fn host_label(machine: &str, mux: &str) -> String {
    if mux.is_empty() {
        return machine.to_string();
    }
    format!("{machine}{MUX_LABEL_SEP}{mux}")
}

/// The path a session is READ as: its host label, then the session, parted by
/// [`MUX_LABEL_SEP`] (`gpu-01/tmux/train-llm`). A session lives on a host and a host on a
/// machine, so its path carries all three levels wherever it is shown; the host id alone
/// would drop the mux of a machine that serves one.
pub fn session_label(machine: &str, mux: &str, session: &str) -> String {
    format!("{}{MUX_LABEL_SEP}{session}", host_label(machine, mux))
}

/// The mux a session is named under: the one its listing reported, else `host_mux`, the
/// mux its host is known by. A listing names the mux it enumerated through, so it is a fact
/// even while the host's own reach is unresolved; every surface that writes a session's
/// path takes its mux from here, so a card and the lines that name the same session cannot
/// spell its mux two ways.
pub fn session_mux<'a>(sess: &'a Session, host_mux: &'a str) -> &'a str {
    if sess.mux.is_empty() {
        host_mux
    } else {
        &sess.mux
    }
}

/// Whether a host's mux may be NAMED on screen, given whether it ANSWERED.
///
/// A mux is named only when it is confirmed. A host that answered enumerated THROUGH its
/// mux, so that mux is a fact about the machine. A host id that names its own mux was
/// resolved from what the machine actually serves, so it is a fact too, and it is the
/// name the user types, so it stays on screen whatever the machine is doing.
///
/// Everything else is a config assumption: a host xmux never reached is offered under the
/// mux it would have tried, and printing that beside a host that answered would put a
/// guess and a fact in one grammar. Such a host is read as its machine alone.
///
/// One function because three surfaces show the pair - a card, the screen it selects, and
/// the doctor's host list - and a rule kept in one of them is a rule the other two
/// drift from.
pub fn mux_may_be_named(host: &str, answered: bool) -> bool {
    answered || !mux_of(host).is_empty()
}

/// The MACHINE half of a host id (`local:zellij` -> `local`, `prod` -> `prod`).
/// Everything before the first [`MUX_SEP`]; an unqualified id is returned whole.
pub fn machine_of(host: &str) -> &str {
    host.split(MUX_SEP).next().unwrap_or(host)
}

/// The MUX half of a host id (`local:zellij` -> `zellij`), or `""` when the id is
/// unqualified because its machine serves a single mux.
pub fn mux_of(host: &str) -> &str {
    host.split_once(MUX_SEP).map_or("", |(_, mux)| mux)
}

/// The distribution a WSL machine name carries (`wsl.Ubuntu-24.04` -> `Ubuntu-24.04`),
/// or `None` for any other machine name. The one spelling of "is this a WSL machine", so
/// no caller re-derives the prefix split - and an empty distro is refused, since
/// `wsl.` alone names no machine.
pub fn wsl_distro_of(machine: &str) -> Option<&str> {
    machine
        .strip_prefix(WSL_PREFIX)
        .filter(|distro| !distro.is_empty())
}

/// True when `host` names a mux on THIS box, qualified or not. The one spelling of
/// "is this local", so no caller compares a host id to [`LOCAL_HOST`] directly and
/// silently stops recognising `local:zellij` as local.
pub fn is_local_host(host: &str) -> bool {
    machine_of(host) == LOCAL_MACHINE
}

/// The state word of a stopped session ([`Session::stopped`]), the one spelling every
/// surface that marks one reads.
pub const STOPPED: &str = "stopped";

/// A host and a session as one value: the pair every internal path carries separately
/// instead of a joined `host/session` string. The joined spelling exists only at the
/// text boundary (the ctl/CLI wire, the persisted file) and at UI render time
/// ([`Address::display`]); code between them carries the two halves.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Address {
    pub host: String,
    pub session: String,
}

impl Address {
    pub fn new(host: impl Into<String>, session: impl Into<String>) -> Self {
        Address {
            host: host.into(),
            session: session.into(),
        }
    }

    /// The joined `host/session` spelling, for UI display and the text wire only.
    pub fn display(&self) -> String {
        format!("{}/{}", self.host, self.session)
    }
}

impl From<&Session> for Address {
    fn from(s: &Session) -> Self {
        Address::new(&s.host, &s.name)
    }
}

/// One mux session as seen on a host.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Session {
    /// `"local"` or an ssh alias.
    pub host: String,
    /// Session name (may contain `/`).
    pub name: String,
    /// The kind of the mux serving this session (`"tmux"` / `"psmux"`), stamped by
    /// the path that enumerated it. Empty when unknown (a parsed target, or a
    /// just-created session awaiting re-enumeration); the nav omits it then.
    pub mux: String,
    /// The mux's own identity for the session, which a rename keeps and a kill followed
    /// by a create never repeats. Empty when the mux's listing carries none, and a
    /// session without one is never read as renamed.
    pub id: String,
    pub windows: i64,
    pub attached: bool,
    /// The mux keeps a record of this session while nothing of it runs: a saved tuios
    /// session, a stopped herdr session, an exited zellij session. The mux's own attach
    /// resumes it, and no other command reaches it until then.
    pub stopped: bool,
}

impl Session {
    /// The cross-environment target as the separate [`Address`] pair.
    pub fn address(&self) -> Address {
        Address::from(self)
    }
}

#[cfg(test)]
mod tests {
    /// A mux is named only when something confirmed it. A host that answered enumerated
    /// through the mux it names; a host that never answered is offered under the mux xmux
    /// WOULD have tried, which is a guess and reads as a fact beside every other label.
    #[test]
    fn an_unanswered_host_with_a_bare_id_names_no_mux() {
        assert!(mux_may_be_named("prod", true), "it answered");
        assert!(!mux_may_be_named("prod", false), "it answered nothing");
    }

    /// An id that names its own mux keeps it whatever the machine is doing: it was
    /// resolved from what that machine serves, and it is the name the user types.
    #[test]
    fn an_id_that_names_its_mux_keeps_it_either_way() {
        assert!(mux_may_be_named("prod:zellij", true));
        assert!(mux_may_be_named("prod:zellij", false));
    }

    use super::*;

    #[test]
    fn host_label_always_parts_the_pair_with_a_slash() {
        assert_eq!(host_label("local", "psmux"), "local/psmux");
        assert_eq!(host_label("prod", "zellij"), "prod/zellij");
        // The id's own separator never reaches a label.
        assert_eq!(
            host_label(machine_of("local:zellij"), mux_of("local:zellij")),
            "local/zellij"
        );
        // Nothing known to name: the machine stands alone rather than trailing a bare
        // separator.
        assert_eq!(host_label("prod", ""), "prod");
    }

    #[test]
    fn address() {
        let s = Session {
            host: "local".into(),
            name: "editor".into(),
            ..Default::default()
        };
        assert_eq!(s.address().host, "local");
        assert_eq!(s.address().session, "editor");
    }

    #[test]
    fn address_is_the_separate_pair_and_displays_joined() {
        let a = Address::new("jup", "api");
        assert_eq!(a.host, "jup");
        assert_eq!(a.session, "api");
        assert_eq!(a.display(), "jup/api");
        // A session name holding a `/` or a space survives as the pair; only the
        // display spelling joins them.
        let b = Address::new("jup", "my/session");
        assert_eq!(b.session, "my/session");
        assert_eq!(b.display(), "jup/my/session");
        assert_eq!(Address::default().display(), "/");
    }

    #[test]
    fn a_host_id_names_its_mux_only_when_the_machine_serves_several() {
        // One mux on a machine keeps the bare alias xmux has always used, so an
        // existing setup's ids, addresses, and typed targets do not move.
        assert_eq!(host_id("local", "psmux", false), "local");
        assert_eq!(host_id("prod", "tmux", false), "prod");
        // Several, and the id says which one this is.
        assert_eq!(host_id("local", "zellij", true), "local:zellij");
        assert_eq!(host_id("prod", "tmux", true), "prod:tmux");
    }

    #[test]
    fn the_machine_half_and_localness_survive_qualification() {
        // Everything that asks "which machine" or "is this machine" must keep working on a
        // qualified id; a bare comparison against LOCAL_HOST would stop seeing
        // `local:zellij` as local.
        assert_eq!(machine_of("local:zellij"), "local");
        assert_eq!(machine_of("prod"), "prod");
        assert_eq!(machine_of(""), "");
        assert!(is_local_host("local"));
        assert!(is_local_host("local:zellij"));
        assert!(!is_local_host("localhost"));
        assert!(!is_local_host("prod:tmux"));
    }

    #[test]
    fn a_wsl_machine_name_carries_its_distribution() {
        // The kind has to be recoverable from the name alone: `machine_of` on any id
        // built over it yields the machine, and that machine says which kind it is.
        assert_eq!(wsl_distro_of("wsl.Ubuntu-24.04"), Some("Ubuntu-24.04"));
        assert_eq!(
            machine_of(&host_id("wsl.Ubuntu-24.04", "zellij", true)),
            "wsl.Ubuntu-24.04"
        );
        // Everything else is not a WSL machine, and neither is the bare prefix.
        assert_eq!(wsl_distro_of("local"), None);
        assert_eq!(wsl_distro_of("prod"), None);
        assert_eq!(
            wsl_distro_of("wsl."),
            None,
            "the prefix alone names nothing"
        );
        assert_eq!(wsl_distro_of("wslx"), None);
    }

    #[test]
    fn a_wsl_machine_is_not_this_box() {
        // A distro's mux registry lives inside the distro, so it must not be taken for
        // this machine's own scope - the local-registry merge would read the wrong registry.
        assert!(!is_local_host("wsl.Ubuntu-24.04"));
        assert!(!is_local_host("wsl.Ubuntu-24.04:zellij"));
    }

    #[test]
    fn a_qualified_host_still_addresses_a_session() {
        // The pair carries the two halves separately, so a qualified host and a
        // zellij session name holding a colon survive as they are - no grammar to
        // re-split.
        let s = Session {
            host: "local:zellij".into(),
            name: "a:b".into(),
            ..Default::default()
        };
        let a = s.address();
        assert_eq!(a.host, "local:zellij");
        assert_eq!(a.session, "a:b");
        assert_eq!(a.display(), "local:zellij/a:b");
    }

    #[test]
    fn local_machine_const() {
        assert_eq!(LOCAL_MACHINE, "local");
    }

    #[test]
    fn address_from_session_round_trips() {
        let s = Session {
            host: "jup".into(),
            name: "api".into(),
            ..Default::default()
        };
        let a: Address = Address::from(&s);
        assert_eq!(a.host, "jup");
        assert_eq!(a.session, "api");
    }
}
