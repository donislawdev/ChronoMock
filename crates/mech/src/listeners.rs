//! The TCP sockets this machine's processes are listening on, with the owning pid of each.
//!
//! An embedded web engine opens its DevTools port inside the session's own process family - the
//! WebView2 browser process, or the host itself for Qt WebEngine - on a port nobody told us. The
//! engine writes the number to a file in its profile, but a file is one engine's convention and it
//! appears when the engine feels like it. The kernel's table is every engine's and it is current:
//! `GetExtendedTcpTable` with the owner-pid listener class lists every LISTEN socket with the pid
//! that bound it, for IPv4 and for IPv6, and that list joined with the family's pids is the port.
//!
//! This reads a table and opens nothing. The connection to a port found here is made by the
//! registered socket code in the CLI, after its loopback-and-port check - see the register in
//! `crates/cli/tests/network.rs`, which names this feature and this reason.

use std::ffi::c_void;

use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID, MIB_TCPROW_OWNER_PID,
    MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
};

/// One listening socket: who bound it, on which port, whether it is bound to a loopback address -
/// the only kind of endpoint the session will ever speak to - and on which address family, because
/// the loopback name to connect to differs between the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Listener {
    pub pid: u32,
    pub port: u16,
    pub loopback: bool,
    pub v6: bool,
}

impl Listener {
    /// The loopback host a client connects to for this listener: `::1` for an IPv6 socket,
    /// `127.0.0.1` otherwise.
    pub fn loopback_host(&self) -> &'static str {
        if self.v6 { "::1" } else { "127.0.0.1" }
    }
}

/// The address families as ws2def.h numbers them. Literal here on purpose: the winsock feature of
/// the windows crate is refused by the network guard, and two documented constants do not need it.
const AF_INET: u32 = 2;
const AF_INET6: u32 = 23;

/// How many times the buffer may be grown when the table changes size between the sizing call and
/// the filling call. Sockets come and go, so one retry is ordinary - a fourth failure is a machine
/// whose table grows faster than it can be read, and that is reported rather than looped on.
const GROW_ATTEMPTS: usize = 4;

/// Every listening TCP socket on the machine, IPv4 and IPv6, each with its owning pid. An error is
/// the API's own word for the caller to report - never a panic, never an empty list dressed as a
/// quiet machine.
pub fn listening_sockets() -> Result<Vec<Listener>, String> {
    let mut out = Vec::new();
    out.extend(read_v4()?);
    out.extend(read_v6()?);
    Ok(out)
}

fn read_v4() -> Result<Vec<Listener>, String> {
    let (buffer, len) = read_table(AF_INET)?;
    // SAFETY: the buffer holds a MIB_TCPTABLE_OWNER_PID the system just wrote, sized by the
    // system's own count, in storage aligned for the structure (see `Table`). The row pointer
    // arithmetic goes through the table's declared array member so padding is the structure's own,
    // as the documentation asks, and the capacity belt keeps the count inside the bytes written.
    unsafe {
        let table = buffer.as_ptr().cast::<MIB_TCPTABLE_OWNER_PID>();
        let count = (*table).dwNumEntries as usize;
        let first = std::ptr::addr_of!((*table).table).cast::<MIB_TCPROW_OWNER_PID>();
        let offset = (first as usize).saturating_sub(table as usize);
        let rows = std::slice::from_raw_parts(
            first,
            count.min(row_capacity(len, offset, std::mem::size_of::<MIB_TCPROW_OWNER_PID>())),
        );
        Ok(rows
            .iter()
            .map(|row| Listener {
                pid: row.dwOwningPid,
                port: port_from_network_order(row.dwLocalPort),
                // dwLocalAddr is in network byte order, so on this little-endian machine the first
                // octet of the address is the low byte of the DWORD: 127 is the whole 127/8 block.
                loopback: (row.dwLocalAddr & 0xFF) == 127,
                v6: false,
            })
            .collect())
    }
}

fn read_v6() -> Result<Vec<Listener>, String> {
    let (buffer, len) = read_table(AF_INET6)?;
    // SAFETY: as in read_v4, over the IPv6 table type.
    unsafe {
        let table = buffer.as_ptr().cast::<MIB_TCP6TABLE_OWNER_PID>();
        let count = (*table).dwNumEntries as usize;
        let first = std::ptr::addr_of!((*table).table).cast::<MIB_TCP6ROW_OWNER_PID>();
        let offset = (first as usize).saturating_sub(table as usize);
        let rows = std::slice::from_raw_parts(
            first,
            count.min(row_capacity(len, offset, std::mem::size_of::<MIB_TCP6ROW_OWNER_PID>())),
        );
        Ok(rows
            .iter()
            .map(|row| Listener {
                pid: row.dwOwningPid,
                port: port_from_network_order(row.dwLocalPort),
                loopback: row.ucLocalAddr == IPV6_LOOPBACK,
                v6: true,
            })
            .collect())
    }
}

/// `::1`, as sixteen bytes in network order.
const IPV6_LOOPBACK: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];

/// How many rows of `row_size` bytes fit in a buffer of `len` bytes past the first `offset` bytes
/// of header. A belt beside the system's own count: a count that claimed more rows than the buffer
/// holds would otherwise read past it.
fn row_capacity(len: usize, offset: usize, row_size: usize) -> usize {
    len.saturating_sub(offset) / row_size.max(1)
}

/// The port as the table stores it - network byte order in the low sixteen bits - back to a number.
fn port_from_network_order(raw: u32) -> u16 {
    u16::from_be((raw & 0xFFFF) as u16)
}

/// Storage for a table the system fills: `u64` cells, so the buffer's alignment is at least the
/// eight bytes either table structure could ask for - a `Vec<u8>` promises an alignment of one,
/// and casting it to a structure with wider fields is undefined behaviour however the allocator
/// happens to behave. The byte length the system wrote travels beside it.
type Table = Vec<u64>;

/// The raw table for one address family and the number of bytes the system wrote into it: size it,
/// then fill it, growing the buffer while the table keeps outgrowing the size it reported a moment
/// earlier.
fn read_table(family: u32) -> Result<(Table, usize), String> {
    let mut size: u32 = 0;
    // SAFETY: a null table with size zero is the documented way to ask for the size.
    let sizing = unsafe { GetExtendedTcpTable(None, &mut size, false, family, TCP_TABLE_OWNER_PID_LISTENER, 0) };
    if WIN32_ERROR(sizing) != ERROR_INSUFFICIENT_BUFFER && WIN32_ERROR(sizing) != ERROR_SUCCESS {
        return Err(format!("GetExtendedTcpTable sizing failed with {sizing}"));
    }
    for _ in 0..GROW_ATTEMPTS {
        let cells = (size as usize).max(4).div_ceil(std::mem::size_of::<u64>());
        let mut buffer: Table = vec![0u64; cells];
        let mut capacity = u32::try_from(buffer.len() * std::mem::size_of::<u64>())
            .map_err(|_| "GetExtendedTcpTable asked for a table larger than the API can address".to_string())?;
        // SAFETY: the buffer holds `capacity` bytes, and the call writes at most that many, updating
        // the size when it needs more.
        let filled = unsafe {
            GetExtendedTcpTable(
                Some(buffer.as_mut_ptr().cast::<c_void>()),
                &mut capacity,
                false,
                family,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        match WIN32_ERROR(filled) {
            ERROR_SUCCESS => return Ok((buffer, capacity as usize)),
            ERROR_INSUFFICIENT_BUFFER => size = capacity,
            other => return Err(format!("GetExtendedTcpTable failed with {}", other.0)),
        }
    }
    Err("GetExtendedTcpTable kept outgrowing its buffer".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_port_in_network_order_reads_back_as_the_number_that_was_bound() {
        // 9222 = 0x2406 - the table holds it as 0x0624 in the low sixteen bits.
        assert_eq!(port_from_network_order(0x0624), 9222);
        assert_eq!(port_from_network_order(0xFFFF_0624), 9222);
        assert_eq!(port_from_network_order(0), 0);
    }

    #[test]
    fn the_loopback_host_follows_the_address_family() {
        let v4 = Listener { pid: 1, port: 1, loopback: true, v6: false };
        let v6 = Listener { pid: 1, port: 1, loopback: true, v6: true };
        assert_eq!(v4.loopback_host(), "127.0.0.1");
        assert_eq!(v6.loopback_host(), "::1");
    }

    #[test]
    fn the_capacity_belt_never_lets_a_count_read_past_the_buffer() {
        let row = std::mem::size_of::<MIB_TCPROW_OWNER_PID>();
        assert_eq!(row_capacity(4 + row * 3, 4, row), 3);
        assert_eq!(row_capacity(4 + row * 3 + row - 1, 4, row), 3);
        assert_eq!(row_capacity(2, 4, row), 0);
        assert_eq!(row_capacity(100, 4, 0), 96);
    }
}
