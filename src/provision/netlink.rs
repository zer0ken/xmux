//! The kernel's own network state, asked for directly: one netlink dump per record.
//!
//! Linux keeps the routing table and the neighbour table behind `NETLINK_ROUTE`, and
//! `ip` is only a program that asks for them. Asking for them here instead of running
//! `ip` removes a process from every probe, works where iproute2 is not installed, and
//! is the only way that works at all on Android: an app there may not `bind` a netlink
//! socket, and iproute2 binds one before it sends anything, so every `ip` command fails
//! identically no matter what it was asked for.
//!
//! A dump needs no bind. The kernel gives the socket a port of its own when the first
//! request goes out, which is why these dumps answer where `ip` cannot. What Android
//! still refuses it refuses per record: the routing table comes back, the neighbour
//! table is denied outright, and this module reports that difference rather than
//! flattening both into an empty list.
//!
//! The messages are packed and read as bytes rather than through C structs. The wire
//! format is kernel ABI - fixed widths, native byte order - and writing it out is what
//! keeps this file the same on every libc, bionic included.

use std::io;
use std::net::Ipv4Addr;

use super::neighbor::{Neighbor, RoutePrefix};

// The netlink constants this module needs. They are kernel ABI, so they are the same
// number on every architecture and every libc; libc names only some of them.
const NETLINK_ROUTE: libc::c_int = 0;
const NLM_F_REQUEST: u16 = 0x001;
const NLM_F_DUMP: u16 = 0x300;
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const RTM_NEWROUTE: u16 = 24;
const RTM_GETROUTE: u16 = 26;
const RTM_NEWNEIGH: u16 = 28;
const RTM_GETNEIGH: u16 = 30;
const RTM_NEWADDR: u16 = 20;
const RTM_GETADDR: u16 = 22;
const IFA_ADDRESS: u16 = 1;
const IFA_LOCAL: u16 = 2;
const RTA_DST: u16 = 1;
const NDA_DST: u16 = 1;
const NDA_LLADDR: u16 = 2;
/// The neighbour states that name no machine: the lookup never completed, or it failed.
const NUD_INCOMPLETE: u16 = 0x01;
const NUD_FAILED: u16 = 0x20;

/// The header every netlink message starts with, and the two fixed bodies used here.
const NLMSGHDR: usize = 16;
const RTMSG: usize = 12;
const NDMSG: usize = 12;
const IFADDRMSG: usize = 8;

/// How long the kernel gets to answer one dump. It answers in microseconds; the budget
/// only stops a blocking thread from waiting on a reply that will never come.
const REPLY_TIMEOUT_SECS: i64 = 3;

/// Every IPv4 destination the routing table names, with the prefix length it names it
/// at. Blocking: call it off the runtime thread.
pub fn routes() -> io::Result<Vec<RoutePrefix>> {
    let mut body = [0u8; RTMSG];
    body[0] = libc::AF_INET as u8; // rtm_family
    let mut out = Vec::new();
    dump(RTM_GETROUTE, &body, RTM_NEWROUTE, |msg| {
        // rtmsg: family, dst_len, src_len, tos, table, ...
        let len = msg[1];
        if msg[0] != libc::AF_INET as u8 {
            return;
        }
        for (kind, val) in attributes(&msg[RTMSG..]) {
            if kind == RTA_DST {
                if let Some(dst) = addr4(val) {
                    out.push(RoutePrefix { dst, len });
                }
            }
        }
    })?;
    Ok(out)
}

/// Every IPv4 address this machine holds, with the prefix length of the network it
/// holds it in. That pair is the link the machine is ON, which is the one network worth
/// asking about when the neighbour table cannot be read. Blocking: call it off the
/// runtime thread.
pub fn addresses() -> io::Result<Vec<RoutePrefix>> {
    let mut body = [0u8; IFADDRMSG];
    body[0] = libc::AF_INET as u8; // ifa_family
    let mut out = Vec::new();
    dump(RTM_GETADDR, &body, RTM_NEWADDR, |msg| {
        // ifaddrmsg: family, prefixlen, flags, scope, index(4)
        let len = msg[1];
        if msg[0] != libc::AF_INET as u8 {
            return;
        }
        // IFA_LOCAL is this machine's own address; on a point-to-point link IFA_ADDRESS
        // is the far end instead, so the local one wins where both are present.
        let mut local = None;
        let mut any = None;
        for (kind, val) in attributes(&msg[IFADDRMSG..]) {
            match kind {
                IFA_LOCAL => local = addr4(val),
                IFA_ADDRESS => any = addr4(val),
                _ => {}
            }
        }
        if let Some(dst) = local.or(any) {
            out.push(RoutePrefix { dst, len });
        }
    })?;
    Ok(out)
}

/// Every IPv4 entry of the neighbour table, with the hardware address that answered for
/// it. An entry whose lookup failed or never completed carries none, which is how the
/// caller tells a machine from an address nothing is at. Blocking: call it off the
/// runtime thread.
pub fn neighbors() -> io::Result<Vec<Neighbor>> {
    let mut body = [0u8; NDMSG];
    body[0] = libc::AF_INET as u8; // ndm_family
    let mut out = Vec::new();
    dump(RTM_GETNEIGH, &body, RTM_NEWNEIGH, |msg| {
        // ndmsg: family, pad, pad, ifindex(4), state(2), flags, type
        let state = u16::from_ne_bytes([msg[8], msg[9]]);
        let dead = state & (NUD_INCOMPLETE | NUD_FAILED) != 0;
        let mut ip = None;
        let mut mac = None;
        for (kind, val) in attributes(&msg[NDMSG..]) {
            match kind {
                NDA_DST => ip = addr4(val),
                NDA_LLADDR if !dead => mac = mac_string(val),
                _ => {}
            }
        }
        if let Some(ip) = ip {
            out.push(Neighbor { ip, mac });
        }
    })?;
    Ok(out)
}

/// Sends one dump request and hands every message of `want` to `on_message`, which
/// receives the message's body: the fixed header is already off it.
///
/// The socket is never bound. Binding is what an app on Android may not do, and a dump
/// does not need it: the first send gets the socket a port from the kernel.
fn dump(request: u16, body: &[u8], want: u16, mut on_message: impl FnMut(&[u8])) -> io::Result<()> {
    let sock = Socket::open()?;
    sock.send_request(request, body)?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = sock.recv(&mut buf)?;
        let mut off = 0usize;
        while off + NLMSGHDR <= n {
            let len = u32::from_ne_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
            let kind = u16::from_ne_bytes(buf[off + 4..off + 6].try_into().unwrap());
            if len < NLMSGHDR || off + len > n {
                return Ok(()); // a truncated tail says nothing more is readable
            }
            match kind {
                NLMSG_DONE => return Ok(()),
                NLMSG_ERROR => {
                    // The body starts with the error as a negative errno; zero is the
                    // acknowledgement of a request that asked for one.
                    let code = i32::from_ne_bytes(
                        buf[off + NLMSGHDR..off + NLMSGHDR + 4].try_into().unwrap(),
                    );
                    return if code == 0 {
                        Ok(())
                    } else {
                        Err(io::Error::from_raw_os_error(-code))
                    };
                }
                k if k == want => on_message(&buf[off + NLMSGHDR..off + len]),
                _ => {}
            }
            off += align4(len);
        }
    }
}

/// The attributes packed after a message's fixed body, as (type, value) pairs.
fn attributes(mut rest: &[u8]) -> Vec<(u16, &[u8])> {
    let mut out = Vec::new();
    while rest.len() >= 4 {
        let len = u16::from_ne_bytes([rest[0], rest[1]]) as usize;
        let kind = u16::from_ne_bytes([rest[2], rest[3]]);
        if len < 4 || len > rest.len() {
            break;
        }
        out.push((kind, &rest[4..len]));
        let step = align4(len).min(rest.len());
        rest = &rest[step..];
    }
    out
}

/// Netlink lengths are rounded up to four bytes, headers and attributes alike.
fn align4(len: usize) -> usize {
    (len + 3) & !3
}

fn addr4(val: &[u8]) -> Option<Ipv4Addr> {
    (val.len() == 4).then(|| Ipv4Addr::new(val[0], val[1], val[2], val[3]))
}

/// A hardware address in the form the neighbour-table parsers already agree on, or
/// `None` for a length that is not one.
fn mac_string(val: &[u8]) -> Option<String> {
    (val.len() == 6).then(|| {
        val.iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(":")
    })
}

/// An owned netlink socket. It exists to close the descriptor on every path out,
/// including the error paths, since the dumps return early on the kernel's word.
struct Socket(libc::c_int);

impl Socket {
    fn open() -> io::Result<Self> {
        // SAFETY: a plain socket(2) with constant arguments.
        let fd = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                NETLINK_ROUTE,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let sock = Socket(fd);
        let tv = libc::timeval {
            tv_sec: REPLY_TIMEOUT_SECS as libc::time_t,
            tv_usec: 0,
        };
        // SAFETY: `tv` outlives the call and its size is what the option expects.
        unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                std::ptr::addr_of!(tv) as *const libc::c_void,
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            );
        }
        Ok(sock)
    }

    /// Sends one request to the kernel (port 0), which is also what binds the socket.
    fn send_request(&self, kind: u16, body: &[u8]) -> io::Result<()> {
        let total = NLMSGHDR + body.len();
        let mut msg = Vec::with_capacity(total);
        msg.extend_from_slice(&(total as u32).to_ne_bytes());
        msg.extend_from_slice(&kind.to_ne_bytes());
        msg.extend_from_slice(&(NLM_F_REQUEST | NLM_F_DUMP).to_ne_bytes());
        msg.extend_from_slice(&1u32.to_ne_bytes()); // sequence
        msg.extend_from_slice(&0u32.to_ne_bytes()); // port: the kernel assigns ours
        msg.extend_from_slice(body);

        let mut kernel: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        kernel.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        // SAFETY: the buffer and the address both outlive the call.
        let sent = unsafe {
            libc::sendto(
                self.0,
                msg.as_ptr() as *const libc::c_void,
                msg.len(),
                0,
                std::ptr::addr_of!(kernel) as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            )
        };
        if sent < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        // SAFETY: the buffer outlives the call and its length is passed with it.
        let n = unsafe { libc::recv(self.0, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(n as usize)
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        // SAFETY: the descriptor is this type's own and is closed once.
        unsafe { libc::close(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attributes_are_read_in_order_and_stop_at_a_bad_length() {
        // Two attributes: type 1 with four bytes, type 2 with two (padded to four).
        let mut buf = Vec::new();
        buf.extend_from_slice(&8u16.to_ne_bytes());
        buf.extend_from_slice(&1u16.to_ne_bytes());
        buf.extend_from_slice(&[10, 0, 0, 1]);
        buf.extend_from_slice(&6u16.to_ne_bytes());
        buf.extend_from_slice(&2u16.to_ne_bytes());
        buf.extend_from_slice(&[7, 7, 0, 0]);
        let got = attributes(&buf);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], (1, &[10, 0, 0, 1][..]));
        assert_eq!(got[1], (2, &[7, 7][..]));

        // A length that runs past the buffer ends the walk instead of reading on.
        let mut bad = Vec::new();
        bad.extend_from_slice(&64u16.to_ne_bytes());
        bad.extend_from_slice(&1u16.to_ne_bytes());
        bad.extend_from_slice(&[10, 0, 0, 1]);
        assert!(attributes(&bad).is_empty());
    }

    #[test]
    fn a_hardware_address_is_read_only_at_its_own_length() {
        assert_eq!(
            mac_string(&[0x00, 0x1a, 0x2b, 0x3c, 0x4d, 0x5e]).as_deref(),
            Some("00:1a:2b:3c:4d:5e")
        );
        assert_eq!(mac_string(&[0, 1, 2]), None, "an infiniband-length address");
    }

    /// The kernel answers a dump on a socket nothing bound. This is the whole reason the
    /// module exists, so it is asserted against the real kernel rather than a fixture.
    #[test]
    fn the_kernel_answers_a_dump_on_an_unbound_socket() {
        let routes = routes().expect("the routing table is readable here");
        assert!(
            routes.iter().any(|r| r.len > 0),
            "a machine with a network has at least one route: {routes:?}"
        );
    }
}
