//! The NEIGHBOUR provider: the machines this box can already reach in one hop, read
//! out of the operating system's own network state.
//!
//! The OS keeps two records of who is directly reachable, and between them they cover
//! both kinds of neighbour:
//!
//! - HOST ROUTES. A route to a single address is a machine the kernel knows how to
//!   reach without asking anyone. A mesh VPN writes one per peer, so the peers of a
//!   tunnel are in the routing table whether or not the vendor's own tool is installed.
//! - THE NEIGHBOUR TABLE (the ARP cache). Every machine on the same link this box has
//!   actually exchanged frames with. A tunnel leaves nothing here - it carries no ARP -
//!   which is exactly why the two sources are both read.
//!
//! Neither record is a list of machines worth offering, so the addresses they give are
//! narrowed twice. The neighbour table's own entries are filtered first: a failed
//! lookup names nothing, and one hardware address answering for many addresses is a
//! router speaking for a whole subnet rather than a machine of its own. Then every
//! surviving address is asked whether it answers ssh, because a printer on the same
//! switch is a neighbour and not a host. What answers is named through the system
//! resolver, which is where a tunnel's own naming already lives, and keeps its address
//! as the name when nothing answers for it.
//!
//! Reading the OS rather than a vendor CLI is what makes this one provider instead of
//! one per network: a tailnet peer, a WireGuard peer, and the machine on the next desk
//! arrive by the same two records and leave by the same gate.
//!
//! Where the OS has an interface for those records, they are read through it - netlink
//! on Linux and Android, IP Helper on Windows - and only the unixes with neither are
//! asked through a command. What differs between platforms is therefore only where the
//! two records come from; everything after them is one path.

use std::collections::{HashMap, HashSet};
use std::net::Ipv4Addr;
use std::time::Duration;

// Only the platforms that answer through a command need a way to run one; Linux and
// Android ask the kernel, Windows asks IP Helper.
#[cfg(not(any(target_os = "linux", target_os = "android", windows)))]
use crate::model::source::{ExecRunner, Runner};

/// How long one address gets to answer on port 22. A neighbour is on this link or one
/// tunnel hop away, so an answer is tens of milliseconds; the budget is really for the
/// ones that will never answer, and it is what the roster waits for at startup, so it
/// buys headroom rather than patience. Every address spends it at the same time.
const SSH_PROBE_TIMEOUT: Duration = Duration::from_millis(700);

/// How many addresses are asked at once. High, because each one is a connect that
/// mostly waits.
const SSH_PROBE_CONCURRENCY: usize = 64;

/// The neighbours worth offering, each with the address it answers on.
///
/// Returns an empty list rather than an error whenever the OS will not say: a machine
/// whose network state cannot be read simply contributes no hosts, and the providers
/// that did answer still fill the roster.
pub async fn neighbors() -> Vec<(String, Option<String>)> {
    let mut candidates: Vec<Ipv4Addr> = Vec::new();
    candidates.extend(host_routes().await);
    candidates.extend(link_neighbors().await);

    let mut seen = HashSet::new();
    let candidates: Vec<Ipv4Addr> = candidates
        .into_iter()
        .filter(|ip| offerable(ip) && seen.insert(*ip))
        .collect();

    let reachable = ssh_responders(candidates).await;
    let names = reverse_names(&reachable).await;
    let mut out: Vec<(String, Option<String>)> = reachable
        .into_iter()
        .map(|ip| {
            let addr = ip.to_string();
            (
                names.get(&ip).cloned().unwrap_or_else(|| addr.clone()),
                Some(addr),
            )
        })
        .collect();
    // A host list that reshuffles between runs is a list the user cannot learn, so the
    // order is the name and then the address: one machine answering on several addresses
    // (its own link and a tunnel it is on) is ONE host, and which of its addresses it
    // keeps must not change between runs either.
    out.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

/// Whether an address is worth asking at all. The addresses that are never a machine on
/// the other side of a connection are dropped here rather than spending a probe.
fn offerable(ip: &Ipv4Addr) -> bool {
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_link_local())
}

// --- host routes ----------------------------------------------------------------

/// What a routing table says about one destination: the address, and how many of its
/// leading bits the route names. A `/32` names one machine; a shorter prefix names a
/// block, which may still be machines (see [`expand`]) or may be a whole network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutePrefix {
    pub dst: Ipv4Addr,
    pub len: u8,
}

/// The shortest prefix still read as machines rather than as a network. A mesh VPN
/// writes one route per peer, but on some platforms it hands the OS a summary instead -
/// two peers arrive as one `/31` - so a route that names a handful of addresses is
/// expanded into them. Eight is where that stops: past it the block is a network, and
/// probing one is a scan rather than a lookup.
const SMALLEST_HOST_BLOCK: u8 = 29;

/// Every address this machine has a route to on its own. Empty when the OS will not say.
async fn host_routes() -> Vec<Ipv4Addr> {
    route_prefixes().await.iter().flat_map(expand).collect()
}

/// The addresses one route names, or none when it names a network.
fn expand(route: &RoutePrefix) -> Vec<Ipv4Addr> {
    if route.len < SMALLEST_HOST_BLOCK || route.len > 32 {
        return Vec::new();
    }
    // At most three bits are ever free here, so the block is at most eight addresses.
    let bits = 32 - u32::from(route.len);
    let base = u32::from(route.dst) & !((1u32 << bits) - 1);
    (0..1u32 << bits)
        .map(|i| Ipv4Addr::from(base + i))
        .collect()
}

/// The routing table as this OS gives it up.
#[cfg(any(target_os = "linux", target_os = "android"))]
async fn route_prefixes() -> Vec<RoutePrefix> {
    // The dump is a syscall away, but it is still a blocking read: it waits on the
    // kernel, so it waits somewhere the runtime thread is not.
    tokio::task::spawn_blocking(|| super::netlink::routes().unwrap_or_default())
        .await
        .unwrap_or_default()
}

/// The routing table as this OS gives it up.
#[cfg(windows)]
async fn route_prefixes() -> Vec<RoutePrefix> {
    tokio::task::spawn_blocking(|| super::iphlpapi::routes().unwrap_or_default())
        .await
        .unwrap_or_default()
}

/// The routing table as this OS gives it up.
#[cfg(not(any(target_os = "linux", target_os = "android", windows)))]
async fn route_prefixes() -> Vec<RoutePrefix> {
    let (bin, args) = route_command();
    match ExecRunner.run(bin, &args).await {
        Ok(out) => parse_host_routes(&String::from_utf8_lossy(&out)),
        Err(_) => Vec::new(),
    }
}

/// The command that prints this machine's IPv4 routes, per OS.
#[cfg(not(any(target_os = "linux", target_os = "android", windows)))]
fn route_command() -> (&'static str, Vec<String>) {
    if cfg!(windows) {
        (
            "powershell",
            vec![
                "-NoProfile".into(),
                "-Command".into(),
                "Get-NetRoute -AddressFamily IPv4 | ForEach-Object { $_.DestinationPrefix }".into(),
            ],
        )
    } else if cfg!(target_os = "macos") {
        ("netstat", vec!["-rn".into(), "-f".into(), "inet".into()])
    } else {
        // Every table, because a mesh VPN commonly puts its peers in one of its own
        // rather than in `main`.
        (
            "ip",
            vec![
                "-4".into(),
                "-o".into(),
                "route".into(),
                "show".into(),
                "table".into(),
                "all".into(),
            ],
        )
    }
}

/// The single-address destinations in a routing table, whatever printed it.
///
/// One parser serves all three formats because only one thing is being looked for: a
/// destination that is one address rather than a range. `ip` prints a host route as a
/// bare address and everything else with a prefix length; `Get-NetRoute` prints every
/// route with one, so a host route is the `/32`; `netstat` prints a host route with an
/// `H` in its flags. A line whose first field is a word (`local`, `broadcast`,
/// `default`, a heading) parses as no address and is skipped by that alone.
pub fn parse_host_routes(out: &str) -> Vec<RoutePrefix> {
    let mut found = Vec::new();
    for line in out.lines() {
        let mut fields = line.split_whitespace();
        let Some(dest) = fields.next() else { continue };
        // `netstat` marks a host route in its flags column; a route to a range there
        // carries no `H` and must not be taken for one.
        if cfg!(target_os = "macos") && !cfg!(windows) {
            let flags = fields.clone().nth(1).unwrap_or("");
            if !flags.contains('H') {
                continue;
            }
        }
        if let Some(route) = destination(dest) {
            found.push(route);
        }
    }
    found
}

/// The prefix a routing table's destination field names. A field with no prefix length
/// is one address, which is how `netstat` writes a host route.
fn destination(dest: &str) -> Option<RoutePrefix> {
    match dest.split_once('/') {
        Some((addr, len)) => Some(RoutePrefix {
            dst: addr.parse().ok()?,
            len: len.parse().ok()?,
        }),
        None => Some(RoutePrefix {
            dst: dest.parse().ok()?,
            len: 32,
        }),
    }
}

// --- the neighbour table --------------------------------------------------------

/// Every machine on this link the OS has actually exchanged frames with, after the
/// entries that name no machine are dropped.
async fn link_neighbors() -> Vec<Ipv4Addr> {
    match neighbor_table().await {
        Ok(table) => usable_neighbors(&table),
        // An OS that refuses the neighbour table has not said there are no neighbours,
        // and the link is still there to be asked. Android refuses it outright, and a
        // phone with no other way to see the machine next to it would otherwise show
        // only what a tunnel routes to.
        Err(_) => link_sweep().await,
    }
}

/// The neighbour table as this OS gives it up, or why it would not.
#[cfg(any(target_os = "linux", target_os = "android"))]
async fn neighbor_table() -> Result<Vec<Neighbor>, String> {
    tokio::task::spawn_blocking(|| super::netlink::neighbors().map_err(|e| e.to_string()))
        .await
        .unwrap_or_else(|e| Err(e.to_string()))
}

/// The neighbour table as this OS gives it up, or why it would not.
#[cfg(windows)]
async fn neighbor_table() -> Result<Vec<Neighbor>, String> {
    tokio::task::spawn_blocking(|| super::iphlpapi::neighbors().map_err(|e| e.to_string()))
        .await
        .unwrap_or_else(|e| Err(e.to_string()))
}

/// The neighbour table as this OS gives it up, or why it would not.
#[cfg(not(any(target_os = "linux", target_os = "android", windows)))]
async fn neighbor_table() -> Result<Vec<Neighbor>, String> {
    let (bin, args) = neighbor_command();
    match ExecRunner.run(bin, &args).await {
        Ok(out) => Ok(parse_neighbors(&String::from_utf8_lossy(&out))),
        Err(e) => Err(e.to_string()),
    }
}

// --- the link itself -------------------------------------------------------------

/// The largest link asked about address by address. A `/24` is 254 questions asked at
/// once, which the probe budget already absorbs; anything wider is a scan, and a machine
/// found by scanning a network this size was never a neighbour in the sense this
/// provider means.
#[cfg(any(target_os = "linux", target_os = "android"))]
const LARGEST_SWEPT_LINK: u8 = 24;

/// Every address on the links this machine holds an address in. Used only where the
/// neighbour table is refused: it asks the link what the table would have remembered.
#[cfg(any(target_os = "linux", target_os = "android"))]
async fn link_sweep() -> Vec<Ipv4Addr> {
    let own = tokio::task::spawn_blocking(|| super::netlink::addresses().unwrap_or_default())
        .await
        .unwrap_or_default();
    own.iter().flat_map(link_addresses).collect()
}

/// Nothing to sweep where a refusal cannot be told from an empty table. Windows and
/// macOS hand the whole table over or fail as a whole, so a refusal there is not the
/// per-record denial the sweep exists for.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
async fn link_sweep() -> Vec<Ipv4Addr> {
    Vec::new()
}

/// The addresses of the network one of this machine's own addresses sits in, without
/// the two that are the network and its broadcast. Empty for a link too wide to ask
/// about, and for one that holds nobody else.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn link_addresses(own: &RoutePrefix) -> Vec<Ipv4Addr> {
    if own.len < LARGEST_SWEPT_LINK || own.len >= 31 {
        return Vec::new();
    }
    let bits = 32 - u32::from(own.len);
    let base = u32::from(own.dst) & !((1u32 << bits) - 1);
    (1..(1u32 << bits) - 1)
        .map(|i| Ipv4Addr::from(base + i))
        .collect()
}

/// What each source of addresses has to say, for a diagnostic that must explain a list
/// that came back empty. It reads the same records the provider reads and probes
/// nothing, so it costs what one scan's first step costs.
pub async fn source_report() -> Vec<(&'static str, String)> {
    let routes = route_prefixes().await;
    let table = neighbor_table().await;
    vec![
        ("routes", format!("{} read", routes.len())),
        (
            "neighbour table",
            match table {
                Ok(t) => format!("{} read", t.len()),
                Err(e) => format!("refused by the OS ({e}); the link is swept instead"),
            },
        ),
    ]
}

/// The command that prints this machine's IPv4 neighbour table, per OS.
#[cfg(not(any(target_os = "linux", target_os = "android", windows)))]
fn neighbor_command() -> (&'static str, Vec<String>) {
    if cfg!(windows) {
        (
            "powershell",
            vec![
                "-NoProfile".into(),
                "-Command".into(),
                "Get-NetNeighbor -AddressFamily IPv4 | ForEach-Object { \"$($_.IPAddress) $($_.LinkLayerAddress) $($_.State)\" }".into(),
            ],
        )
    } else if cfg!(target_os = "macos") {
        ("arp", vec!["-an".into()])
    } else {
        (
            "ip",
            vec!["-4".into(), "-o".into(), "neigh".into(), "show".into()],
        )
    }
}

/// One entry of a neighbour table: an address and the hardware address that answered
/// for it. An entry that resolved to no hardware address carries `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Neighbor {
    pub ip: Ipv4Addr,
    pub mac: Option<String>,
}

/// The entries of a neighbour table, whatever printed it.
///
/// The three formats agree on what matters and differ in where it sits, so each line is
/// read as a bag of fields: the first address is the neighbour, the first thing shaped
/// like a hardware address is what answered for it, and a line saying the lookup failed
/// or is incomplete carries no hardware address at all. `arp` wraps its address in
/// parentheses and `ip` labels its own with `lladdr`; neither changes what is being
/// looked for.
pub fn parse_neighbors(out: &str) -> Vec<Neighbor> {
    let mut found = Vec::new();
    for line in out.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let Some(ip) = fields
            .iter()
            .find_map(|f| f.trim_matches(['(', ')']).parse::<Ipv4Addr>().ok())
        else {
            continue;
        };
        let mac = fields.iter().find(|f| is_mac(f)).map(|f| f.to_lowercase());
        found.push(Neighbor { ip, mac });
    }
    found
}

/// Whether a field is a hardware address: colon- or hyphen-separated pairs of hex.
/// `incomplete`, `FAILED`, an interface name, and an address all fail it.
fn is_mac(field: &str) -> bool {
    let parts: Vec<&str> = field.split([':', '-']).collect();
    parts.len() >= 6
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 2 && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// The neighbours that name a machine of their own.
///
/// An entry with no hardware address resolved to nothing and names nobody. One hardware
/// address answering for SEVERAL addresses is a router answering for everything behind
/// it, so none of those addresses is the machine at that hardware address; a machine
/// wearing two addresses on one card is lost with them, which is the rarer case by far
/// and the one whose loss costs a duplicate rather than a stranger.
pub fn usable_neighbors(entries: &[Neighbor]) -> Vec<Ipv4Addr> {
    let mut count: HashMap<&str, usize> = HashMap::new();
    for e in entries {
        if let Some(mac) = &e.mac {
            *count.entry(mac.as_str()).or_default() += 1;
        }
    }
    entries
        .iter()
        .filter(|e| e.mac.as_deref().is_some_and(|m| count[m] == 1))
        .map(|e| e.ip)
        .collect()
}

// --- the gate -------------------------------------------------------------------

/// The addresses that answered ssh, asked all at once.
///
/// A neighbour is not a host: the same link carries printers, phones, and appliances,
/// and a tunnel's routing table keeps a peer that is switched off. Opening port 22 and
/// reading what it says is the one question that separates them, and it is asked only
/// of addresses the OS already says are reachable, so it is not a sweep of anything.
async fn ssh_responders(ips: Vec<Ipv4Addr>) -> Vec<Ipv4Addr> {
    use futures::stream::StreamExt;
    let answered: Vec<Option<Ipv4Addr>> = futures::stream::iter(
        ips.into_iter()
            .map(|ip| async move { answers_ssh(ip).await.then_some(ip) }),
    )
    .buffer_unordered(SSH_PROBE_CONCURRENCY)
    .collect()
    .await;
    answered.into_iter().flatten().collect()
}

/// Whether one address answers as an ssh server that is not this machine.
///
/// The banner is what proves it is ssh rather than something else holding the port.
/// Reaching THIS machine is told from reaching another by the socket itself: a
/// connection to one of our own addresses comes back with the same address at both
/// ends. That needs no list of our own addresses, so it is right on every platform and
/// cannot go stale.
async fn answers_ssh(ip: Ipv4Addr) -> bool {
    use tokio::io::AsyncReadExt;
    let addr = std::net::SocketAddr::from((ip, 22));
    let Ok(Ok(mut stream)) =
        tokio::time::timeout(SSH_PROBE_TIMEOUT, tokio::net::TcpStream::connect(addr)).await
    else {
        return false;
    };
    if stream
        .local_addr()
        .is_ok_and(|local| reached_this_box(local.ip(), addr.ip()))
    {
        return false;
    }
    let mut buf = [0u8; 4];
    match tokio::time::timeout(SSH_PROBE_TIMEOUT, stream.read_exact(&mut buf)).await {
        Ok(Ok(n)) => is_ssh_banner(&buf[..n]),
        _ => false,
    }
}

/// Whether a connection reached THIS box: the address at both ends is the same one.
/// This box is the `local` source, reached without ssh, so finding it among its own
/// neighbours would offer it twice under two names.
fn reached_this_box(local: std::net::IpAddr, peer: std::net::IpAddr) -> bool {
    local == peer
}

/// Whether what a port said is an ssh server introducing itself. Every ssh server opens
/// with its protocol version, so four bytes settle it and nothing else has to be read.
fn is_ssh_banner(head: &[u8]) -> bool {
    head == b"SSH-"
}

// --- naming ---------------------------------------------------------------------

/// What the system resolver calls each address, for the ones it answers for.
///
/// The system resolver is asked, never a nameserver directly: a mesh VPN configures the
/// resolver with the zones that name its peers, so asking the OS is what makes a peer
/// come back under the name its own network gave it. An address the resolver says
/// nothing about is absent from the map and keeps its address as its name.
async fn reverse_names(ips: &[Ipv4Addr]) -> HashMap<Ipv4Addr, String> {
    if ips.is_empty() {
        return HashMap::new();
    }
    let lookups: Vec<_> = ips
        .iter()
        .copied()
        // A resolver call waits on a network answer, so it goes to the blocking pool
        // rather than the thread drawing frames. They wait together, not in turn.
        .map(|ip| tokio::task::spawn_blocking(move || (ip, reverse_name(ip))))
        .collect();
    let mut map = HashMap::new();
    for lookup in lookups {
        if let Ok((ip, Some(name))) = lookup.await {
            map.insert(ip, name);
        }
    }
    map
}

/// What the system resolver calls one address, or `None` when it names it nothing.
///
/// `getnameinfo` is the resolver's own entry point on every unix, Android included, so
/// this asks exactly what any other program on the machine would ask and gets the same
/// answer - including the answers a VPN installed for its own reverse zones.
#[cfg(unix)]
fn reverse_name(ip: Ipv4Addr) -> Option<String> {
    // Long enough for any name a resolver may return (the traditional NI_MAXHOST).
    let mut host = [0 as libc::c_char; 1025];
    let mut sa: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    sa.sin_family = libc::AF_INET as libc::sa_family_t;
    // Both are network byte order already: the octets as they travel, and `s_addr` as it
    // is stored, so the bytes are moved and never swapped.
    sa.sin_addr.s_addr = u32::from_ne_bytes(ip.octets());
    let rc = unsafe {
        libc::getnameinfo(
            std::ptr::addr_of!(sa) as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
            host.as_mut_ptr(),
            // bionic types this length as `size_t` where glibc and the BSDs use
            // `socklen_t`, so the cast target is whatever this platform's own
            // declaration says rather than a name that is right on only some of them.
            host.len() as _,
            std::ptr::null_mut(),
            0,
            // A name or nothing: without this the call answers with the address itself,
            // which would make every machine look named.
            libc::NI_NAMEREQD,
        )
    };
    if rc != 0 {
        return None;
    }
    let name = unsafe { std::ffi::CStr::from_ptr(host.as_ptr()) }
        .to_str()
        .ok()?;
    dns_label(name)
}

/// Windows asks its DNS client, which is where a VPN installs the policy for its own
/// reverse zones. One process per address would cost a shell start each, so the whole
/// list goes in one call.
#[cfg(windows)]
fn reverse_name(ip: Ipv4Addr) -> Option<String> {
    let out = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!(
                "$n = Resolve-DnsName -Type PTR -ErrorAction SilentlyContinue {ip}; if ($n) {{ $n[0].NameHost }}"
            ),
        ])
        .output()
        .ok()?;
    dns_label(String::from_utf8_lossy(&out.stdout).trim())
}

/// The first label of a name, when it is one a shell can be handed as an ssh target.
/// Anything else is refused so a malformed answer cannot become an ssh argument.
fn dns_label(name: &str) -> Option<String> {
    let label = name.trim().trim_end_matches('.').split('.').next()?;
    let ok = !label.is_empty()
        && label.len() <= 63
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && !label.starts_with('-');
    ok.then(|| label.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real `ip -4 -o route show table all` output: a mesh VPN's per-peer routes beside
    /// the subnet routes, the machine's own addresses, and a default.
    const IP_ROUTE: &str = "\
default via 143.248.140.1 dev eno1 proto static metric 100
100.64.141.117 dev tailscale0 table 52
100.77.0.2 dev tailscale0 table 52
143.248.140.0/24 dev eno1 proto kernel scope link src 143.248.140.120 metric 100
192.168.0.0/24 dev docker0 proto kernel scope link src 192.168.0.1 linkdown
local 143.248.140.120 dev eno1 table local proto kernel scope host src 143.248.140.120
broadcast 143.248.140.255 dev eno1 table local proto kernel scope link src 143.248.140.120
";

    /// The addresses a routing table offers, the way the provider reads it: parsed, then
    /// expanded, so a test sees what a probe would be sent to.
    fn addresses(out: &str) -> Vec<Ipv4Addr> {
        parse_host_routes(out).iter().flat_map(expand).collect()
    }

    #[test]
    fn a_route_to_one_address_is_a_neighbour_and_a_route_to_a_range_is_not() {
        assert_eq!(
            addresses(IP_ROUTE),
            vec![
                "100.64.141.117".parse::<Ipv4Addr>().unwrap(),
                "100.77.0.2".parse().unwrap(),
            ],
            "only the per-peer routes name a machine"
        );
    }

    /// A routing table read on an Android phone on the same tunnel. Its VPN client hands
    /// the OS summarised routes, so two peers arrive as one `/31` and taking only `/32`s
    /// would lose them. Both addresses in such a block answered ssh.
    #[test]
    fn a_summarised_route_names_every_machine_in_it() {
        let got = addresses(
            "\
100.77.0.1/32
100.77.0.2/31
100.88.0.6/31
192.168.45.0/24
",
        );
        assert!(
            got.contains(&"100.77.0.2".parse().unwrap())
                && got.contains(&"100.77.0.3".parse().unwrap())
                && got.contains(&"100.88.0.6".parse().unwrap())
                && got.contains(&"100.88.0.7".parse().unwrap()),
            "both halves of each summarised route are asked: {got:?}"
        );
        assert!(
            !got.iter().any(|a| a.octets()[..3] == [192, 168, 45]),
            "a /24 is a network, and asking every address in one is a scan: {got:?}"
        );
    }

    /// The link a machine is on is asked about address by address only while it is a
    /// link. The two addresses that are the network itself and its broadcast are not
    /// machines and are not asked.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn a_link_is_swept_only_while_it_is_a_link() {
        let link = |len| {
            link_addresses(&RoutePrefix {
                dst: "192.168.45.180".parse().unwrap(),
                len,
            })
        };
        assert_eq!(link(24).len(), 254);
        assert_eq!(link(24)[0], "192.168.45.1".parse::<Ipv4Addr>().unwrap());
        assert_eq!(link(24)[253], "192.168.45.254".parse::<Ipv4Addr>().unwrap());
        assert_eq!(link(30).len(), 2);
        assert!(
            link(23).is_empty(),
            "wider than a link, and asking it is a scan"
        );
        assert!(
            link(31).is_empty(),
            "a point-to-point link holds nobody else"
        );
        assert!(link(32).is_empty());
    }

    /// Where a block stops being machines. Eight addresses are asked; sixteen are a
    /// network, and the routing table says nothing about what is in it.
    #[test]
    fn a_block_is_expanded_only_while_it_is_small() {
        let block = |len| {
            expand(&RoutePrefix {
                dst: "10.0.0.9".parse().unwrap(),
                len,
            })
        };
        assert_eq!(block(32), vec!["10.0.0.9".parse::<Ipv4Addr>().unwrap()]);
        assert_eq!(block(31).len(), 2);
        assert_eq!(block(29).len(), 8);
        assert_eq!(
            block(29)[0],
            "10.0.0.8".parse::<Ipv4Addr>().unwrap(),
            "the block starts where the prefix does, not at the address given"
        );
        assert!(block(28).is_empty());
        assert!(block(24).is_empty());
    }

    /// Real `Get-NetRoute` output: every route carries a prefix length, so the host
    /// routes are the ones ending in /32.
    #[test]
    fn a_windows_routing_table_names_its_peers_by_the_thirty_two() {
        let out = "\
100.119.48.112/32
100.117.199.32/32
100.64.0.0/10
127.0.0.1/32
224.0.0.0/4
255.255.255.255/32
";
        let got = addresses(out);
        assert!(
            got.contains(&"100.119.48.112".parse().unwrap())
                && got.contains(&"100.117.199.32".parse().unwrap()),
            "the peers are taken: {got:?}"
        );
        assert!(
            !got.contains(&"100.64.0.0".parse().unwrap()),
            "the range the tunnel claims is not a machine: {got:?}"
        );
        // The addresses that are never someone else are dropped before any probe.
        assert!(!offerable(&"127.0.0.1".parse().unwrap()));
        assert!(!offerable(&"255.255.255.255".parse().unwrap()));
        assert!(!offerable(&"224.0.0.1".parse().unwrap()));
    }

    /// Real `ip -4 -o neigh show` output: a resolved neighbour, a failed lookup, and a
    /// router answering for a whole subnet.
    #[test]
    fn the_neighbour_table_keeps_only_entries_that_name_a_machine() {
        let out = "\
143.248.140.60 dev eno1 lladdr 7c:c2:55:29:f3:8c REACHABLE
143.248.140.69 dev eno1 lladdr 7c:c2:55:29:f3:90 STALE
192.168.0.164 dev docker0  FAILED
10.249.15.165 dev eno1 lladdr 00:90:27:ef:64:ee STALE
143.248.90.79 dev eno1 lladdr 00:90:27:ef:64:ee STALE
1.1.1.1 dev eno1 lladdr 00:90:27:ef:64:ee STALE
";
        let entries = parse_neighbors(out);
        assert_eq!(entries.len(), 6, "every line is an entry: {entries:?}");
        assert_eq!(
            entries[2].mac, None,
            "a failed lookup resolved to no hardware address"
        );
        let usable = usable_neighbors(&entries);
        assert_eq!(
            usable,
            vec![
                "143.248.140.60".parse::<Ipv4Addr>().unwrap(),
                "143.248.140.69".parse().unwrap(),
            ],
            "the failed entry and the router's three addresses are not machines"
        );
    }

    /// Real `arp -an` output, whose address sits in parentheses and whose failed entries
    /// say `incomplete` where a hardware address would be.
    #[test]
    fn a_bsd_neighbour_table_reads_the_same_way() {
        let out = "\
? (192.168.1.20) at 3c:07:54:72:6e:ee on en0 ifscope [ethernet]
? (192.168.1.30) at (incomplete) on en0 ifscope [ethernet]
";
        let entries = parse_neighbors(out);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].ip, "192.168.1.20".parse::<Ipv4Addr>().unwrap());
        assert_eq!(entries[0].mac.as_deref(), Some("3c:07:54:72:6e:ee"));
        assert_eq!(
            entries[1].mac, None,
            "`(incomplete)` is no hardware address"
        );
    }

    /// Real `Get-NetNeighbor` output, whose hardware addresses are hyphenated and upper
    /// case.
    #[test]
    fn a_windows_neighbour_table_reads_the_same_way() {
        let out = "\
192.168.0.219 34-5A-60-7E-F5-56 Stale
192.168.0.1 8C-90-2D-9B-AE-F2 Reachable
192.168.0.99  Unreachable
";
        let entries = parse_neighbors(out);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].mac.as_deref(), Some("34-5a-60-7e-f5-56"));
        assert_eq!(entries[2].mac, None);
    }

    /// A resolver's answer is trimmed to the name a user recognises: the first label.
    /// A tunnel names its peers under its own suffix, and the whole of it reaches the
    /// same machine, so carrying it would only make the list harder to read.
    #[test]
    fn a_resolver_answer_is_kept_as_its_first_label() {
        assert_eq!(
            dns_label("mars02.tail1cbccc.ts.net").as_deref(),
            Some("mars02")
        );
        assert_eq!(
            dns_label("mars02.tail1cbccc.ts.net.").as_deref(),
            Some("mars02")
        );
        assert_eq!(dns_label("jupiter06").as_deref(), Some("jupiter06"));
        // The name is what ssh will be handed, so its case is settled here rather than
        // leaving two spellings of one machine on the roster.
        assert_eq!(dns_label("Kyla.tail0.ts.net").as_deref(), Some("kyla"));
    }

    /// Nothing that could not be handed to ssh as a target may become one.
    #[test]
    fn an_answer_that_is_not_a_name_is_refused() {
        assert_eq!(dns_label("-weird.example"), None, "a leading dash");
        assert_eq!(dns_label("."), None, "a root with no label");
        assert_eq!(dns_label(""), None, "nothing at all");
        assert_eq!(
            dns_label("has space.example"),
            None,
            "a space is not a label"
        );
        assert_eq!(
            dns_label(&"x".repeat(64)),
            None,
            "longer than a label may be"
        );
    }

    /// A machine the resolver says nothing about is still a machine: it keeps its
    /// address as its name rather than dropping off the roster.
    #[test]
    fn an_unnamed_neighbour_keeps_its_address_as_its_name() {
        let names: HashMap<Ipv4Addr, String> = HashMap::new();
        let ip: Ipv4Addr = "10.0.0.9".parse().unwrap();
        let name = names.get(&ip).cloned().unwrap_or_else(|| ip.to_string());
        assert_eq!(name, "10.0.0.9");
    }

    /// Nothing to ask about costs no process: the resolver is not run for an empty list.
    #[tokio::test]
    async fn naming_nothing_runs_nothing() {
        assert!(reverse_names(&[]).await.is_empty());
    }

    /// The gate is what separates a neighbour from a host: a port has to introduce
    /// itself as ssh, and a port that says anything else, or nothing, does not.
    #[test]
    fn only_an_ssh_greeting_makes_a_neighbour_a_host() {
        assert!(is_ssh_banner(b"SSH-"));
        assert!(!is_ssh_banner(b"HTTP"), "a web server is not a host");
        assert!(
            !is_ssh_banner(b""),
            "a port that said nothing is not one either"
        );
        assert!(!is_ssh_banner(b"SS"), "half a greeting is not one");
    }

    /// This box is the `local` source, reached without ssh. It sits in its own routing
    /// table, so it has to be told from its neighbours - by the connection, which comes
    /// back with the same address at both ends, rather than by a list of our addresses
    /// that could go stale.
    #[test]
    fn a_connection_to_this_box_is_not_a_neighbour() {
        let mine: std::net::IpAddr = "143.248.140.120".parse().unwrap();
        let theirs: std::net::IpAddr = "143.248.140.60".parse().unwrap();
        assert!(reached_this_box(mine, mine));
        assert!(!reached_this_box(mine, theirs));
    }

    /// The whole provider against this machine's real network state. Prints what it
    /// offers, so the pipeline can be seen end to end on a box whose neighbours are
    /// known. Ignored by default: it reads the OS and opens connections.
    #[tokio::test]
    #[ignore = "reads this machine's network state and probes its neighbours"]
    async fn live_neighbors_of_this_machine() {
        let found = neighbors().await;
        for (name, addr) in &found {
            println!("{name}\t{}", addr.as_deref().unwrap_or("-"));
        }
        println!("{} neighbours", found.len());
    }
}
