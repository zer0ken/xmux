//! Asking a machine its own name, when nothing else knows it.
//!
//! A reverse DNS lookup answers only for an address someone registered a name for. A
//! tunnel registers every peer it manages, so its peers all have names; the machine on
//! the next desk usually has none, and a card for it can only show its address.
//!
//! That machine still knows its own name, and a machine running mDNS answers for it.
//! mDNS is DNS's message format with no server in it: the question goes to the machine
//! rather than to a registry, and what comes back is what the machine calls itself.
//! Asking costs one UDP packet, needs no credentials, and happens before any connection,
//! which is what makes it a name for a card the user has not logged in to yet.
//!
//! The question here is sent to the address directly rather than to the multicast group.
//! A unicast question is answered by the one machine being asked, so an answer is
//! already attributed - there is nothing to match up afterwards, and a machine that runs
//! no responder simply refuses the port.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::Duration;

/// The port mDNS listens on, asked directly rather than through the group address.
const MDNS_PORT: u16 = 5353;

/// How long one machine gets to say its own name. It is on this link or one tunnel hop
/// away and answers from memory, so this is the budget for the ones that never will.
const ANSWER_TIMEOUT: Duration = Duration::from_millis(400);

/// What the machine at `ip` calls itself, or `None` when it does not answer.
///
/// Blocking: one send and one receive on a UDP socket. Call it off the runtime thread.
pub fn own_name(ip: Ipv4Addr) -> Option<String> {
    let question = reverse_question(ip);
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.set_read_timeout(Some(ANSWER_TIMEOUT)).ok()?;
    socket
        .send_to(&question, SocketAddr::from((ip, MDNS_PORT)))
        .ok()?;
    let mut buf = [0u8; 1500];
    let (n, from) = socket.recv_from(&mut buf).ok()?;
    // Only the machine that was asked may answer for itself.
    if from.ip() != ip {
        return None;
    }
    first_pointer_name(&buf[..n])
}

/// A PTR question for the address, in the `in-addr.arpa` form every responder knows.
fn reverse_question(ip: Ipv4Addr) -> Vec<u8> {
    let o = ip.octets();
    let name = format!("{}.{}.{}.{}.in-addr.arpa", o[3], o[2], o[1], o[0]);
    let mut msg = Vec::with_capacity(name.len() + 18);
    msg.extend_from_slice(&[0x00, 0x00]); // id: a unicast answer needs no matching
    msg.extend_from_slice(&[0x00, 0x00]); // flags: a plain question
    msg.extend_from_slice(&[0x00, 0x01]); // one question
    msg.extend_from_slice(&[0x00; 6]); // no answers, authorities, or extras
    for label in name.split('.') {
        msg.push(label.len() as u8);
        msg.extend_from_slice(label.as_bytes());
    }
    msg.push(0); // the root label ends the name
    msg.extend_from_slice(&[0x00, 0x0c]); // type PTR
    msg.extend_from_slice(&[0x00, 0x01]); // class IN
    msg
}

/// The name in the first PTR answer of a DNS message, or `None` when it carries none.
fn first_pointer_name(msg: &[u8]) -> Option<String> {
    if msg.len() < 12 {
        return None;
    }
    let questions = u16::from_be_bytes([msg[4], msg[5]]);
    let answers = u16::from_be_bytes([msg[6], msg[7]]);
    let mut at = 12;
    for _ in 0..questions {
        at = skip_name(msg, at)?;
        at = at.checked_add(4)?; // the type and class of the question
    }
    for _ in 0..answers {
        at = skip_name(msg, at)?;
        let kind = u16::from_be_bytes([*msg.get(at)?, *msg.get(at + 1)?]);
        let len = u16::from_be_bytes([*msg.get(at + 8)?, *msg.get(at + 9)?]) as usize;
        let data = at.checked_add(10)?;
        if data.checked_add(len)? > msg.len() {
            return None;
        }
        if kind == 12 {
            return read_name(msg, data).map(|(name, _)| name);
        }
        at = data + len;
    }
    None
}

/// Where the name starting at `at` ends. A name is a run of labels, and the run may end
/// in a pointer back into the message instead of a root label.
fn skip_name(msg: &[u8], mut at: usize) -> Option<usize> {
    loop {
        let len = *msg.get(at)? as usize;
        if len == 0 {
            return Some(at + 1);
        }
        if len & 0xc0 == 0xc0 {
            return Some(at + 2); // a pointer is the last thing in a name
        }
        at = at.checked_add(1 + len)?;
    }
}

/// The name starting at `at`, following the pointers a message uses to say a suffix
/// once. Bounded by a hop count, because a message may point in a circle.
fn read_name(msg: &[u8], mut at: usize) -> Option<(String, usize)> {
    let mut labels: Vec<String> = Vec::new();
    let mut hops = 0;
    loop {
        let len = *msg.get(at)? as usize;
        if len == 0 {
            return Some((labels.join("."), at + 1));
        }
        if len & 0xc0 == 0xc0 {
            hops += 1;
            if hops > 8 {
                return None;
            }
            let next = ((len & 0x3f) << 8) | *msg.get(at + 1)? as usize;
            at = next;
            continue;
        }
        let start = at + 1;
        let end = start.checked_add(len)?;
        labels.push(String::from_utf8_lossy(msg.get(start..end)?).into_owned());
        at = end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_question_asks_for_the_address_backwards() {
        let q = reverse_question("192.168.45.180".parse().unwrap());
        let text = String::from_utf8_lossy(&q).into_owned();
        assert!(
            text.contains("180") && text.contains("in-addr") && text.contains("arpa"),
            "the question names the address in reverse: {text:?}"
        );
        assert_eq!(&q[4..6], &[0, 1], "exactly one question");
        assert_eq!(&q[q.len() - 4..], &[0, 12, 0, 1], "a PTR question in IN");
    }

    /// A real answer from a machine running avahi: one question echoed back, then the
    /// PTR naming the machine. The name is read out of the answer rather than the
    /// question, which is what a compressed message makes easy to get wrong.
    #[test]
    fn the_name_is_read_out_of_the_answer() {
        let mut msg = reverse_question("1.0.0.11".parse().unwrap());
        msg[2] = 0x84; // a response, authoritative
        msg[6] = 0x00;
        msg[7] = 0x01; // one answer
        msg.extend_from_slice(&[0xc0, 0x0c]); // the answer's name points at the question
        msg.extend_from_slice(&[0x00, 0x0c, 0x00, 0x01]); // PTR, IN
        msg.extend_from_slice(&[0x00, 0x00, 0x00, 0x78]); // ttl
        let name: &[u8] = &[
            9, b'j', b'u', b'p', b'i', b't', b'e', b'r', b'0', b'0', 5, b'l', b'o', b'c', b'a',
            b'l', 0,
        ];
        msg.extend_from_slice(&(name.len() as u16).to_be_bytes());
        msg.extend_from_slice(name);
        assert_eq!(first_pointer_name(&msg).as_deref(), Some("jupiter00.local"));
    }

    #[test]
    fn a_message_with_no_answer_names_nothing() {
        let q = reverse_question("10.0.0.1".parse().unwrap());
        assert_eq!(first_pointer_name(&q), None);
        assert_eq!(first_pointer_name(&[]), None);
        assert_eq!(first_pointer_name(&[0; 12]), None);
    }

    /// A name that points at itself is a message that would be read forever.
    #[test]
    fn a_circular_name_ends() {
        let msg = [0u8; 12]
            .into_iter()
            .chain([0xc0, 0x0c])
            .collect::<Vec<u8>>();
        assert_eq!(read_name(&msg, 12), None);
    }
}
