//! Listening sockets from the IP Helper API.
//!
//! `GetExtendedTcpTable` and `GetExtendedUdpTable` are the same calls the
//! portable provider reaches through a crate, minus the intermediate `Vec` of
//! every socket on the machine — this filters while it walks the table.
//!
//! Parsing `netstat -ano` is not an option and never was: its output is
//! localised, so the state column reads `LISTENING` on an English install and
//! something else everywhere it matters.

use std::mem::{offset_of, size_of};
use std::net::{Ipv4Addr, Ipv6Addr};

use runtime_adapter::port::{PortBinding, PortProvider, Protocol};
use runtime_types::{Result, RuntimeError};
use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, NO_ERROR};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, GetExtendedUdpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID,
    MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID, MIB_TCP_STATE_LISTEN, MIB_UDP6ROW_OWNER_PID,
    MIB_UDP6TABLE_OWNER_PID, MIB_UDPROW_OWNER_PID, MIB_UDPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_ALL,
    UDP_TABLE_OWNER_PID,
};
use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};

#[derive(Debug, Default)]
pub struct WindowsPortProvider;

impl WindowsPortProvider {
    pub fn new() -> Self {
        Self
    }
}

/// Which table to ask for.
#[derive(Clone, Copy)]
enum Table {
    Tcp4,
    Tcp6,
    Udp4,
    Udp6,
}

impl PortProvider for WindowsPortProvider {
    fn listening_ports(&self) -> Result<Vec<PortBinding>> {
        let mut bindings: Vec<PortBinding> = Vec::new();
        for table in [Table::Tcp4, Table::Tcp6, Table::Udp4, Table::Udp6] {
            for row in read_table(table)? {
                add(&mut bindings, row);
            }
        }
        // TCP first within a port, so callers taking the first match get the
        // protocol they almost always mean.
        bindings.sort_by(|a, b| {
            a.port
                .cmp(&b.port)
                .then_with(|| a.protocol.cmp(&b.protocol))
                .then_with(|| a.address.cmp(&b.address))
        });
        Ok(bindings)
    }

    fn is_port_free(&self, port: u16) -> Result<bool> {
        // Bind-probe rather than trusting the socket table: it also catches
        // ports held in a state the table does not report as listening. TCP
        // only — reserving a port for a dev server means a TCP port.
        match std::net::TcpListener::bind(("127.0.0.1", port)) {
            Ok(listener) => {
                drop(listener);
                Ok(self.binding_for(port)?.is_none())
            }
            Err(_) => Ok(false),
        }
    }
}

/// A forking server binds once and shares the socket, so the same port
/// legitimately appears under several pids.
fn add(bindings: &mut Vec<PortBinding>, row: PortBinding) {
    match bindings.iter_mut().find(|existing| {
        existing.port == row.port
            && existing.protocol == row.protocol
            && existing.address == row.address
    }) {
        Some(existing) => {
            for pid in row.pids {
                if !existing.pids.contains(&pid) {
                    existing.pids.push(pid);
                }
            }
        }
        None => bindings.push(row),
    }
}

/// Ports come back in network byte order, in the low half of a `u32`.
fn port_of(raw: u32) -> u16 {
    u16::from_be_bytes([(raw & 0xff) as u8, ((raw >> 8) & 0xff) as u8])
}

fn read_table(table: Table) -> Result<Vec<PortBinding>> {
    let buffer = fetch(table)?;
    Ok(match table {
        Table::Tcp4 => {
            parse::<MIB_TCPROW_OWNER_PID>(&buffer, offset_of!(MIB_TCPTABLE_OWNER_PID, table))
                .into_iter()
                .filter(|row| row.dwState == MIB_TCP_STATE_LISTEN as u32)
                .map(|row| PortBinding {
                    port: port_of(row.dwLocalPort),
                    protocol: Protocol::Tcp,
                    address: Ipv4Addr::from(row.dwLocalAddr.to_ne_bytes()).to_string(),
                    pids: vec![row.dwOwningPid],
                })
                .collect()
        }
        Table::Tcp6 => {
            parse::<MIB_TCP6ROW_OWNER_PID>(&buffer, offset_of!(MIB_TCP6TABLE_OWNER_PID, table))
                .into_iter()
                .filter(|row| row.dwState == MIB_TCP_STATE_LISTEN as u32)
                .map(|row| PortBinding {
                    port: port_of(row.dwLocalPort),
                    protocol: Protocol::Tcp,
                    address: Ipv6Addr::from(row.ucLocalAddr).to_string(),
                    pids: vec![row.dwOwningPid],
                })
                .collect()
        }
        // UDP has no connection state: a bound socket is already receiving.
        Table::Udp4 => {
            parse::<MIB_UDPROW_OWNER_PID>(&buffer, offset_of!(MIB_UDPTABLE_OWNER_PID, table))
                .into_iter()
                .map(|row| PortBinding {
                    port: port_of(row.dwLocalPort),
                    protocol: Protocol::Udp,
                    address: Ipv4Addr::from(row.dwLocalAddr.to_ne_bytes()).to_string(),
                    pids: vec![row.dwOwningPid],
                })
                .collect()
        }
        Table::Udp6 => {
            parse::<MIB_UDP6ROW_OWNER_PID>(&buffer, offset_of!(MIB_UDP6TABLE_OWNER_PID, table))
                .into_iter()
                .map(|row| PortBinding {
                    port: port_of(row.dwLocalPort),
                    protocol: Protocol::Udp,
                    address: Ipv6Addr::from(row.ucLocalAddr).to_string(),
                    pids: vec![row.dwOwningPid],
                })
                .collect()
        }
    })
}

/// Ask for the size, allocate, then ask for the table.
///
/// The retry loop is not paranoia: sockets open and close between the two
/// calls, so the size the first call reports can already be too small.
fn fetch(table: Table) -> Result<Vec<u32>> {
    let (family, class): (u32, i32) = match table {
        Table::Tcp4 => (AF_INET as u32, TCP_TABLE_OWNER_PID_ALL),
        Table::Tcp6 => (AF_INET6 as u32, TCP_TABLE_OWNER_PID_ALL),
        Table::Udp4 => (AF_INET as u32, UDP_TABLE_OWNER_PID),
        Table::Udp6 => (AF_INET6 as u32, UDP_TABLE_OWNER_PID),
    };
    let udp = matches!(table, Table::Udp4 | Table::Udp6);

    let mut size: u32 = 0;
    // Backed by `u32` so the buffer is aligned for the MIB structs, whose
    // fields are all `u32` or byte arrays.
    let mut buffer: Vec<u32> = Vec::new();

    for _ in 0..8 {
        let pointer = if buffer.is_empty() {
            std::ptr::null_mut()
        } else {
            buffer.as_mut_ptr().cast()
        };
        // SAFETY: `pointer` is either null with `size` 0, which is how the API
        // is asked for the size it needs, or a buffer of exactly `size` bytes.
        let code = unsafe {
            if udp {
                GetExtendedUdpTable(pointer, &mut size, 0, family, class, 0)
            } else {
                GetExtendedTcpTable(pointer, &mut size, 0, family, class, 0)
            }
        };
        match code {
            code if code == NO_ERROR => {
                return Ok(buffer);
            }
            code if code == ERROR_INSUFFICIENT_BUFFER => {
                buffer = vec![0u32; (size as usize).div_ceil(size_of::<u32>()).max(1)];
            }
            code => {
                return Err(RuntimeError::io(format!(
                    "failed to read the socket table: error {code}"
                )))
            }
        }
    }
    Err(RuntimeError::io(
        "the socket table kept growing between calls".to_string(),
    ))
}

/// Read `dwNumEntries` rows starting at `rows_at`.
fn parse<T: Copy>(buffer: &[u32], rows_at: usize) -> Vec<T> {
    let Some(&count) = buffer.first() else {
        return Vec::new();
    };
    let bytes = std::mem::size_of_val(buffer);
    let mut out = Vec::with_capacity(count as usize);
    for index in 0..count as usize {
        let at = rows_at + index * size_of::<T>();
        // The API is trusted for the count, but a short buffer would be a read
        // past the end; stop rather than fault.
        if at + size_of::<T>() > bytes {
            break;
        }
        // SAFETY: `at` is in bounds by the check above, and `read_unaligned`
        // makes no alignment assumption about the row's position.
        let row = unsafe { buffer.as_ptr().cast::<u8>().add(at).cast::<T>().read_unaligned() };
        out.push(row);
    }
    out
}
