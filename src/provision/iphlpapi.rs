//! Windows' own network state, asked for directly: one IP Helper call per record.
//!
//! Windows keeps the routing table and the neighbour table behind `iphlpapi`, and
//! `Get-NetRoute` and `Get-NetNeighbor` are cmdlets that call it. Calling it here costs
//! no PowerShell start per probe, cannot be stopped by an execution policy or a slow
//! profile, and hands back rows rather than text to be parsed back into rows.
//!
//! This is the Windows half of the same seam Linux and Android reach over netlink; the
//! two hand the caller the same shapes so everything above them is one path.

use std::io;
use std::net::Ipv4Addr;

use windows_sys::Win32::Foundation::NO_ERROR;
use windows_sys::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetIpForwardTable2, GetIpNetTable2, MIB_IPFORWARD_TABLE2, MIB_IPNET_TABLE2,
};
use windows_sys::Win32::Networking::WinSock::{
    NlnsIncomplete, NlnsUnreachable, ADDRESS_FAMILY, AF_INET, SOCKADDR_INET,
};

use super::neighbor::{Neighbor, RoutePrefix};

/// Every IPv4 destination the routing table names, with the prefix length it names it
/// at. Blocking: it calls into the OS, so call it off the runtime thread.
pub fn routes() -> io::Result<Vec<RoutePrefix>> {
    let mut table: *mut MIB_IPFORWARD_TABLE2 = std::ptr::null_mut();
    // SAFETY: the call fills `table` with a block it allocated, freed below on every
    // path out. `AF_INET` is the family the rows are then read as.
    let rc = unsafe { GetIpForwardTable2(AF_INET as ADDRESS_FAMILY, &mut table) };
    if rc != NO_ERROR {
        return Err(io::Error::from_raw_os_error(rc as i32));
    }
    let mut out = Vec::new();
    // SAFETY: `Table` is a variable-length array of `NumEntries` rows, which is how the
    // API declares it; the pointer stays valid until `FreeMibTable`.
    unsafe {
        let rows =
            std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize);
        for row in rows {
            if let Some(dst) = addr4(&row.DestinationPrefix.Prefix) {
                out.push(RoutePrefix {
                    dst,
                    len: row.DestinationPrefix.PrefixLength,
                });
            }
        }
        FreeMibTable(table as *const core::ffi::c_void);
    }
    Ok(out)
}

/// Every IPv4 entry of the neighbour table, with the hardware address that answered for
/// it. An entry whose lookup failed or never completed carries none. Blocking: call it
/// off the runtime thread.
pub fn neighbors() -> io::Result<Vec<Neighbor>> {
    let mut table: *mut MIB_IPNET_TABLE2 = std::ptr::null_mut();
    // SAFETY: as above - the OS allocates, this function frees.
    let rc = unsafe { GetIpNetTable2(AF_INET as ADDRESS_FAMILY, &mut table) };
    if rc != NO_ERROR {
        return Err(io::Error::from_raw_os_error(rc as i32));
    }
    let mut out = Vec::new();
    // SAFETY: as above.
    unsafe {
        let rows =
            std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize);
        for row in rows {
            let Some(ip) = addr4(&row.Address) else {
                continue;
            };
            let dead = row.State == NlnsUnreachable || row.State == NlnsIncomplete;
            let len = row.PhysicalAddressLength as usize;
            let mac = (!dead && len == 6).then(|| {
                row.PhysicalAddress[..6]
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<Vec<_>>()
                    .join(":")
            });
            out.push(Neighbor { ip, mac });
        }
        FreeMibTable(table as *const core::ffi::c_void);
    }
    Ok(out)
}

/// The IPv4 address in one of the API's address unions, or `None` when the row is about
/// another family. The union is read only after its own family field says which arm is
/// live, which is what makes reading it defined.
fn addr4(sa: &SOCKADDR_INET) -> Option<Ipv4Addr> {
    // SAFETY: `si_family` overlaps the family field of every arm, so it is readable
    // whichever arm is live, and `Ipv4` is read only once it says `AF_INET`.
    unsafe {
        if sa.si_family != AF_INET as ADDRESS_FAMILY {
            return None;
        }
        // The address is stored in network order, which is the order the octets travel.
        Some(Ipv4Addr::from(sa.Ipv4.sin_addr.S_un.S_addr.to_ne_bytes()))
    }
}
