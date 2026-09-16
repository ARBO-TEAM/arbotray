//! Which ports this machine has open, and which process owns each one.
//!
//! Two Win32 tables give this: `GetExtendedTcpTable` and `GetExtendedUdpTable`,
//! both asked for the *owner PID* class, which is the one that carries the
//! `dwOwningPid` column. That column is the whole reason this is useful — a list
//! of bare port numbers is a puzzle, and a port beside the name of the program
//! holding it is an answer.
//!
//! IPv4 only. The v6 tables are a separate query with 16-byte address columns
//! and the same port/pid columns, so they would double the work for rows the
//! page cannot tell apart from their v4 twins — a listener on `:445` is one
//! door, whichever stack is behind it.

use crate::telemetry::PortInfo;
use std::collections::HashMap;
use std::ffi::c_void;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, GetExtendedUdpTable, MIB_TCPROW_OWNER_PID, MIB_UDPROW_OWNER_PID,
    TCP_TABLE_OWNER_PID_ALL, UDP_TABLE_OWNER_PID,
};
use windows::Win32::Networking::WinSock::AF_INET;
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    QueryFullProcessImageNameW, TerminateProcess,
};
use windows::core::PWSTR;

/// `GetExtendedTcpTable` and friends return a Win32 error code, not a `BOOL`.
/// Zero is the success case, and the only one worth spelling out.
const ERROR_SUCCESS: u32 = 0;

/// What the *sizing* call answers, and the reason it is not `ERROR_SUCCESS`.
///
/// Asking for a null buffer is the documented way to be told how much room the
/// table needs, and the call reports that by failing with this code and writing
/// the size. Treating it as a failure is silent — every table comes back empty
/// and the page reads `0` for a machine that has a hundred sockets open, which
/// is a plausible-looking answer rather than an error. It cost this module
/// exactly that: `the_open_ports_are_real_ones` is what caught it.
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;

/// `MIB_TCP_STATE_LISTEN` and `MIB_TCP_STATE_ESTAB`, as the numbers the table
/// actually carries.
///
/// Spelled as the raw `i32` rather than matched against the `MIB_TCP_STATE`
/// constants: those are a newtype that does not derive `PartialEq` and so
/// cannot be used in a match arm, and a `u32`→`i32` cast is not a comparison.
/// `the_state_numbers_match_the_sdk` pins them against the real constants, so a
/// typo here fails a test rather than quietly reporting every socket as Other.
const STATE_LISTEN: i32 = 2;
const STATE_ESTABLISHED: i32 = 5;

/// How many pid→name answers to keep before starting over.
///
/// A desktop runs a few dozen processes and a busy server a few hundred; past
/// this the map is worth more as freed memory than as hit rate, and rebuilding
/// it costs one `OpenProcess` per pid on the next tick.
const NAME_CACHE_MAX: usize = 512;

/// The most listening ports the page lists. A machine with a container runtime
/// or a dev server stack can hold a hundred open ports, and a hundred rows is
/// not a page — it is a scroll that does not scroll.
pub const MAX_OPEN_ROWS: usize = 12;

/// Reads the two port tables. Holds the pid→name cache between polls.
#[derive(Default)]
pub struct Ports {
    /// Process names, by pid. A name cannot change while a pid is alive, and
    /// resolving one costs an `OpenProcess` and a path read — once per tick for
    /// the same dozen pids is work for a string already known.
    names: HashMap<u32, String>,
}

impl Ports {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every open endpoint, summarised.
    ///
    /// Always answers: an empty table is a machine with nothing open, which is
    /// a different reading from a failure but not one worth a `None` for — the
    /// counts are all zero either way, and the page draws them.
    pub fn poll(&mut self) -> PortsSample {
        let tcp = tcp_rows();
        let udp = udp_rows();

        let mut listeners = 0u32;
        let mut established = 0u32;
        // Listening ports, deduped: a socket bound to both `0.0.0.0` and a
        // specific address is one open door, not two.
        let mut open: Vec<PortInfo> = Vec::new();
        let mut seen: Vec<u16> = Vec::new();

        for row in &tcp {
            match tcp_state(row.dwState) {
                Some(TcpState::Listen) => {
                    listeners += 1;
                    let port = port_of(row.dwLocalPort);
                    if seen.contains(&port) {
                        continue;
                    }
                    seen.push(port);
                    open.push(self.endpoint(port, Some(TcpState::Listen), row.dwOwningPid));
                }
                Some(TcpState::Established) => established += 1,
                // A transient socket on its way out. Counted as neither.
                None => {}
            }
        }

        // The doors a reader actually cares about first, and the ephemeral
        // range (49152+) last: a listening port above it is almost always a
        // program that took whatever was free, not a service anyone chose.
        open.sort_by_key(|p| (p.port >= 49_152, p.port));
        open.truncate(MAX_OPEN_ROWS);

        let mut owners: Vec<u32> = tcp
            .iter()
            .map(|r| r.dwOwningPid)
            .chain(udp.iter().map(|r| r.dwOwningPid))
            .filter(|pid| *pid != 0)
            .collect();
        owners.sort_unstable();
        owners.dedup();

        PortsSample {
            listeners,
            established,
            // UDP rows are one per bound socket and carry no state at all, so
            // they are a count and never a row: `:5353` bound and idle tells a
            // reader nothing a listener does not tell them better.
            udp: udp.len() as u32,
            owners: owners.len() as u32,
            open,
        }
    }

    /// One endpoint, with its owner resolved through the cache.
    fn endpoint(&mut self, port: u16, state: Option<TcpState>, pid: u32) -> PortInfo {
        PortInfo {
            port,
            state,
            pid,
            process: self.name_of(pid),
        }
    }

    /// The owning executable's file name, by pid.
    fn name_of(&mut self, pid: u32) -> Option<String> {
        if pid == 0 {
            // The system idle process. It owns sockets and cannot be opened, so
            // asking would be a guaranteed failure dressed as a lookup.
            return None;
        }
        if let Some(name) = self.names.get(&pid) {
            return Some(name.clone());
        }
        let name = process_name(pid);
        if self.names.len() >= NAME_CACHE_MAX {
            self.names.clear();
        }
        if let Some(name) = &name {
            self.names.insert(pid, name.clone());
        }
        name
    }
}

/// The TCP half of a sample, mapped only as far as the page distinguishes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpState {
    Listen,
    Established,
}

/// One poll of the two port tables.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PortsSample {
    /// TCP sockets in LISTEN — the machine's open doors.
    pub listeners: u32,
    /// TCP sockets with a peer.
    pub established: u32,
    /// Bound UDP sockets.
    pub udp: u32,
    /// Distinct processes holding any of the above.
    pub owners: u32,
    /// The listening ports to draw, most-interesting first, already capped.
    pub open: Vec<PortInfo>,
}

/// The number the state column holds, mapped onto the two the page shows.
///
/// Every other state — `TIME_WAIT`, `CLOSE_WAIT`, `SYN_SENT` — collapses to
/// `None`: they are transient sockets on their way out, and counting them
/// separately would make the summary move on every tick for no reading.
fn tcp_state(raw: u32) -> Option<TcpState> {
    match raw as i32 {
        STATE_LISTEN => Some(TcpState::Listen),
        STATE_ESTABLISHED => Some(TcpState::Established),
        _ => None,
    }
}

/// The port out of a table column.
///
/// The column is a `DWORD` whose low 16 bits hold the port in network byte
/// order and whose high 16 bits are padding. Both halves matter: masking to the
/// low half and *not* swapping reads `:445` as `:47873`.
fn port_of(raw: u32) -> u16 {
    u16::from_be((raw & 0xFFFF) as u16)
}

/// The rows of the IPv4 TCP owner-PID table, or empty when the call fails.
fn tcp_rows() -> Vec<MIB_TCPROW_OWNER_PID> {
    table(TCP_TABLE_OWNER_PID_ALL.0, false)
}

/// The rows of the IPv4 UDP owner-PID table, or empty when the call fails.
fn udp_rows() -> Vec<MIB_UDPROW_OWNER_PID> {
    table(UDP_TABLE_OWNER_PID.0, true)
}

/// Ask one of the two extended tables for its rows.
///
/// Both calls have the same shape: the first asks how much room the answer
/// needs (the documented way is a null buffer), the second fills it. The two
/// share this function because the sizes and the failure modes are identical;
/// only the row type differs, and that is the caller's `transmute` to make.
///
/// The backing buffer is a `Vec<u32>` rather than the `Vec<u8>` these usually
/// get. Both row types are four-byte-aligned structs and the count is a `u32`,
/// so a byte buffer would be a struct read from a one-byte-aligned address —
/// undefined behaviour that usually works and is not worth the coin flip.
fn table<T: Copy>(table_class: i32, udp: bool) -> Vec<T> {
    let class = windows::Win32::NetworkManagement::IpHelper::TCP_TABLE_CLASS(table_class);
    let udp_class = windows::Win32::NetworkManagement::IpHelper::UDP_TABLE_CLASS(table_class);
    let mut size = 0u32;

    // SAFETY: a null buffer is the documented way to be told the size, and
    // `size` is a live local.
    let rc = unsafe {
        if udp {
            GetExtendedUdpTable(None, &mut size, false, AF_INET.0 as u32, udp_class, 0)
        } else {
            GetExtendedTcpTable(None, &mut size, false, AF_INET.0 as u32, class, 0)
        }
    };
    // Either code means "here is the size": the buffer-filling answer when the
    // table happened to be empty, and the short-buffer one for every table that
    // is not. Anything else is a real failure.
    if (rc != ERROR_SUCCESS && rc != ERROR_INSUFFICIENT_BUFFER) || size == 0 {
        return Vec::new();
    }

    // One word of slack: `size` is a byte count and the buffer is indexed in
    // words, so a size that is not a multiple of four rounds up.
    let mut buf: Vec<u32> = vec![0; (size as usize).div_ceil(4) + 1];
    // SAFETY: the buffer is at least `size` bytes wide, which the first call
    // established, and both class values came from the caller's constant.
    let rc = unsafe {
        if udp {
            GetExtendedUdpTable(
                Some(buf.as_mut_ptr() as *mut c_void),
                &mut size,
                false,
                AF_INET.0 as u32,
                udp_class,
                0,
            )
        } else {
            GetExtendedTcpTable(
                Some(buf.as_mut_ptr() as *mut c_void),
                &mut size,
                false,
                AF_INET.0 as u32,
                class,
                0,
            )
        }
    };
    if rc != ERROR_SUCCESS {
        return Vec::new();
    }

    // Both tables are `{ DWORD dwNumEntries; ROW table[]; }`, so the count is
    // the first word and the rows begin four bytes in — still word-aligned.
    let count = buf[0] as usize;
    let row_bytes = size_of::<T>();
    // The count is the table's word count. A table that filled the buffer
    // exactly, or a lying count, must not read past the allocation.
    let room = buf.len().saturating_sub(1) * size_of::<u32>() / row_bytes.max(1);
    let count = count.min(room);
    let rows = buf.as_ptr().wrapping_add(1) as *const T;
    // SAFETY: `count` rows of `T` fit in the words after the leading count, the
    // buffer is word-aligned, and every row type here is `Copy` plain data.
    (0..count)
        .map(|i| unsafe { rows.add(i).read() })
        .collect()
}

/// End the process holding a port. `true` when it is gone.
///
/// This is the one destructive call in the app, and it is deliberately the
/// narrowest one Windows offers: `TerminateProcess` on a single pid, opened
/// with the one right that call needs and nothing else. No tree walk, no
/// `taskkill /T /F` — a monitor that takes a service's children down with it
/// is a monitor that can end a session the user did not point at.
///
/// A refusal is the common answer rather than the exceptional one: every
/// protected process — the system, the antivirus, anything running as another
/// user — refuses this handle, and a monitor that is not elevated cannot
/// terminate them. `false` means "not allowed", which the caller reports as
/// itself rather than dressing up as a failure of the machine.
pub fn stop_process(pid: u32) -> bool {
    // Pid 0 is the system idle process and 4 is the kernel. Neither is a
    // running program, both own sockets, and terminating either is not a thing
    // that can happen — so this refuses before Windows does, with the caller
    // getting the same `false` it would have got anyway.
    if pid <= 4 {
        return false;
    }
    // SAFETY: `pid` came from a table row. The handle is checked, the call is
    // made only with a live one, and it is closed on every path out.
    let handle: HANDLE = match unsafe { OpenProcess(PROCESS_TERMINATE, false, pid) } {
        Ok(h) => h,
        Err(_) => return false,
    };
    // SAFETY: a live handle opened for exactly this right; the exit code is an
    // argument, not a read.
    let ok = unsafe { TerminateProcess(handle, 1) }.is_ok();
    // SAFETY: opened above, closed exactly once.
    unsafe {
        let _ = CloseHandle(handle);
    }
    ok
}

/// The file name of the process with `pid`, or `None` if it cannot be opened.
///
/// Fails routinely and must not be treated as an error: the overwhelming
/// majority of sockets on a healthy machine are held by protected processes,
/// and `OpenProcess` is refused for every one of them. A missing name is the
/// normal case, so the page draws a placeholder rather than dropping the row.
fn process_name(pid: u32) -> Option<String> {
    // SAFETY: `pid` came from a table row; the handle is checked and closed on
    // every path out of this function.
    let handle: HANDLE =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;

    // A full image path is bounded by MAX_PATH for the vast majority of
    // executables; one that is longer is reported as a truncated path, which
    // still yields the right file name because the truncation is at the tail.
    let mut buf = [0u16; 260];
    let mut len = buf.len() as u32;
    // SAFETY: the handle is live, the buffer is `len` wide, and `len` is
    // updated by the call with what it wrote.
    let ok = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    };
    // SAFETY: opened above, closed exactly once.
    unsafe {
        let _ = CloseHandle(handle);
    }
    ok.ok()?;

    let path = String::from_utf16_lossy(&buf[..len as usize]);
    // The file name, not the path: `C:\Windows\System32\svchost.exe` is a wall
    // of text that says the same thing as `svchost.exe` once it is in a row
    // beside a port number.
    Some(
        path.rsplit(['\\', '/'])
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(&path)
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::NetworkManagement::IpHelper::{
        MIB_TCP_STATE_ESTAB, MIB_TCP_STATE_LISTEN,
    };

    /// The two state numbers are raw `i32`s above because the SDK constants
    /// cannot appear in a match arm. This is the check that they still agree
    /// with the SDK: if a future bindings release renumbers them, `tcp_state`
    /// would classify every socket as `None` and the summary would read zero
    /// listeners on a listening machine.
    #[test]
    fn the_state_numbers_match_the_sdk() {
        assert_eq!(MIB_TCP_STATE_LISTEN.0, STATE_LISTEN);
        assert_eq!(MIB_TCP_STATE_ESTAB.0, STATE_ESTABLISHED);
    }

    #[test]
    fn the_port_is_read_out_of_the_low_half_in_network_order() {
        // 445 is 0x01BD, which is stored big-endian in the low half: 0xBD01.
        assert_eq!(port_of(0x0000_BD01), 445);
        // 80 -> 0x0050 -> 0x5000 in the low half.
        assert_eq!(port_of(0x0000_5000), 80);
        // The high half is padding and must not reach the result — reading it
        // is what turns 445 into 47873.
        assert_eq!(port_of(0xFFFF_BD01), 445);
        assert_eq!(port_of(0), 0);
    }

    #[test]
    fn transient_states_are_not_counted() {
        assert_eq!(tcp_state(STATE_LISTEN as u32), Some(TcpState::Listen));
        assert_eq!(
            tcp_state(STATE_ESTABLISHED as u32),
            Some(TcpState::Established)
        );
        // TIME_WAIT (11) and CLOSE_WAIT (8) are sockets on their way out.
        assert_eq!(tcp_state(11), None);
        assert_eq!(tcp_state(8), None);
    }

    /// The collector runs against this machine. It is here for the same reason
    /// the disk and volume tests are: the Win32 call is the part that cannot be
    /// unit-tested from a literal, so the check is that the live table is a
    /// *plausible* one rather than a specific one.
    #[test]
    fn the_open_ports_are_real_ones() {
        let sample = Ports::new().poll();

        // Every running Windows has a listening port and a bound UDP socket.
        assert!(sample.listeners > 0, "no TCP listener on a running machine");
        assert!(sample.udp > 0, "no bound UDP socket on a running machine");
        assert!(sample.owners > 0, "sockets with no owning process");

        // The list is capped and never repeats a port.
        assert!(sample.open.len() <= MAX_OPEN_ROWS);
        let mut ports: Vec<u16> = sample.open.iter().map(|p| p.port).collect();
        let found = ports.len();
        ports.sort_unstable();
        ports.dedup();
        assert_eq!(found, ports.len(), "the same port is listed twice");

        // Every row is a listener with a port in range, and a pid that either
        // resolved to a file name or was left as a placeholder.
        for row in &sample.open {
            assert_eq!(row.state, Some(TcpState::Listen));
            assert!(row.port > 0, "port 0 is not a door");
            assert!(row.pid > 0, "a listener with no owning process");
            if let Some(name) = &row.process {
                assert!(!name.is_empty(), "an empty process name");
                assert!(
                    !name.contains('\\'),
                    "{name} is a path, not a file name"
                );
            }
        }
    }

    /// The same pid must not be opened twice for a name already known.
    #[test]
    fn a_name_is_resolved_once_per_pid() {
        let mut ports = Ports::new();
        let own = std::process::id();
        // This process can always be opened, so the first call resolves and the
        // second has to come out of the cache.
        let first = ports.name_of(own);
        assert!(first.is_some(), "a process cannot name itself");
        assert_eq!(ports.names.get(&own), first.as_ref());
        assert_eq!(ports.name_of(own), first);
    }
}
