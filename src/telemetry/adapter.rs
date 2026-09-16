//! Adapter detail collector: the interface Windows is actually routing through,
//! the address it holds, and the resolvers it was handed.
//!
//! `network` sums the octet counters of *every* up interface, which is the right
//! answer to "how fast is this machine talking" and no answer at all to "over
//! what". This is the other half: one adapter, chosen the way Windows itself
//! chooses one, so the address and the resolvers on the page belong to the same
//! interface the traffic is flowing through.
//!
//! Nothing is cached between polls. The table changes underneath us (a cable, a
//! VPN, a DHCP lease) and the whole read is one API call plus a walk of a
//! handful of nodes — cheaper than deciding when a cached copy went stale.

use super::AdapterSample;
use std::mem::size_of;
use windows::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_SUCCESS};
use windows::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_MULTICAST, GET_ADAPTERS_ADDRESSES_FLAGS,
    GetAdaptersAddresses, GetBestInterfaceEx, IF_TYPE_SOFTWARE_LOOPBACK, IF_TYPE_TUNNEL,
    IP_ADAPTER_ADDRESSES_LH, IP_ADAPTER_DNS_SERVER_ADDRESS_XP, IP_ADAPTER_UNICAST_ADDRESS_LH,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Networking::WinSock::{
    AF_INET, AF_UNSPEC, SOCKADDR, SOCKADDR_IN, SOCKET_ADDRESS,
};
use windows::core::PWSTR;

/// Everything the page shows and nothing it does not: unicast records (the
/// local address) and DNS stay, while anycast and multicast records — bytes in
/// the buffer nothing here ever reads — are skipped.
const FLAGS: GET_ADAPTERS_ADDRESSES_FLAGS =
    GET_ADAPTERS_ADDRESSES_FLAGS(GAA_FLAG_SKIP_ANYCAST.0 | GAA_FLAG_SKIP_MULTICAST.0);

/// One row of the adapter table, reduced to what the selection rule needs.
///
/// `node` is the real row in the buffer; it is only ever dereferenced while that
/// buffer is alive, and the pure tests below build rows with a null node and
/// never read it.
struct Row {
    node: *const IP_ADAPTER_ADDRESSES_LH,
    if_index: u32,
    if_type: u32,
    up: bool,
}

/// Which row to report.
///
/// The first choice is the interface Windows would send a packet to 8.8.8.8
/// through — the same target `latency` probes — because that is the interface
/// whose counters are the ones moving. When that call fails, or names a row
/// that is not usable, the first up *physical* interface is a better answer than
/// nothing; loopback is never a connection, and a tunnel's own address is a stub
/// nobody recognises, so neither is eligible for the fallback.
fn choose_row(rows: &[Row], want: Option<u32>) -> Option<usize> {
    let usable = |row: &Row| row.up && row.if_type != IF_TYPE_SOFTWARE_LOOPBACK;
    if let Some(index) = want {
        // The routed row is taken even if it is a tunnel: if the traffic really
        // goes through one, that is the address the user is looking at.
        if let Some(i) = rows.iter().position(|r| usable(r) && r.if_index == index) {
            return Some(i);
        }
    }
    rows.iter()
        .position(|r| usable(r) && r.if_type != IF_TYPE_TUNNEL)
}

/// The interface index a packet to 8.8.8.8 would leave by, or `None` on an
/// IPv4-less stack.
fn routed_interface() -> Option<u32> {
    // All four octets are 8, so this literal is byte-order-proof as well as
    // being the same destination `latency` pings.
    let mut dest = SOCKADDR_IN {
        sin_family: AF_INET,
        ..Default::default()
    };
    dest.sin_addr.S_un.S_addr = u32::from_be_bytes([8, 8, 8, 8]);

    let mut index = 0u32;
    // SAFETY: `dest` is a live, correctly-typed `sockaddr_in`; the call reads
    // only its family and address, and writes one `u32` into `index`.
    let rc = unsafe {
        GetBestInterfaceEx((&dest as *const SOCKADDR_IN).cast::<SOCKADDR>(), &mut index)
    };
    (rc == ERROR_SUCCESS.0 && index != 0).then_some(index)
}

/// The second call: same query, now with a buffer to fill into.
///
/// # Safety
/// `buf` must be a live allocation of at least `*size` bytes, and `size` the
/// number the size-probe call asked for.
unsafe fn fill(buf: &mut [u8], size: &mut u32) -> u32 {
    // SAFETY: the caller guarantees the buffer and the size it holds.
    unsafe {
        GetAdaptersAddresses(
            AF_UNSPEC.0 as u32,
            FLAGS,
            None,
            Some(buf.as_mut_ptr().cast()),
            size,
        )
    }
}

/// A `PWSTR` the API owns, copied out before the table goes away.
///
/// # Safety
/// `p` must be null, or point at a NUL-terminated wide string that outlives
/// this call — which every string in an adapter table does.
unsafe fn pwstr_string(p: PWSTR) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees a readable NUL-terminated string.
    let text = unsafe { p.to_string() }.ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// The IPv4 an address record holds, or `None` for any other family.
///
/// This is the whole byte-order story for adapters: the field's *bytes* are the
/// address and its numeric value is not, so the raw value is handed back
/// untouched and formatted by `crate::taskbar::format_addr` — the one function
/// that reads a Win32 address. Formatting here as well would be a second place
/// to get the direction wrong.
///
/// # Safety
/// `addr.lpSockaddr` must be null, or point to a `SOCKADDR` of at least
/// `iSockaddrLength` bytes — the API's own guarantee for every record it fills.
unsafe fn v4_of(addr: &SOCKET_ADDRESS) -> Option<u32> {
    let sa = addr.lpSockaddr;
    if sa.is_null() || (addr.iSockaddrLength as usize) < size_of::<SOCKADDR_IN>() {
        return None;
    }
    // SAFETY: the callers pass records the API filled, so the length check
    // above is satisfied and the family decides how the bytes are read.
    unsafe {
        if (*sa).sa_family != AF_INET {
            return None;
        }
        Some((*sa.cast::<SOCKADDR_IN>()).sin_addr.S_un.S_addr)
    }
}

/// The first IPv4 address on a chain.
///
/// IPv6 entries are skipped rather than shown: every adapter has a link-local
/// `fe80::` address whether or not anything is reachable, so a page full of them
/// is noise, and the IPv4 is the one a user can compare against `ipconfig`.
///
/// # Safety
/// `head` must be null, or the head of a chain owned by a live adapter table.
unsafe fn first_v4(head: *const IP_ADAPTER_UNICAST_ADDRESS_LH) -> Option<u32> {
    let mut node = head;
    // SAFETY: the caller guarantees every node on the chain is live.
    unsafe {
        while let Some(record) = node.as_ref() {
            if let Some(v4) = v4_of(&record.Address) {
                return Some(v4);
            }
            node = record.Next;
        }
    }
    None
}

/// Every IPv4 resolver on the chain, in the order Windows returns them — which
/// is the order they are tried in, so it is worth keeping.
///
/// # Safety
/// As `first_v4`.
unsafe fn dns_servers(head: *const IP_ADAPTER_DNS_SERVER_ADDRESS_XP) -> Vec<u32> {
    let mut out = Vec::new();
    let mut node = head;
    // SAFETY: the caller guarantees every node on the chain is live.
    unsafe {
        while let Some(record) = node.as_ref() {
            if let Some(v4) = v4_of(&record.Address) {
                out.push(v4);
            }
            node = record.Next;
        }
    }
    out
}

/// One walk of the adapter table, or `None` if it could not be read at all.
fn read() -> Option<AdapterSample> {
    let want = routed_interface();

    let mut size = 0u32;
    // SAFETY: the first call passes no buffer, which is the documented way to
    // ask for the size — the API writes nothing but `size` and answers with
    // ERROR_BUFFER_OVERFLOW. The table is then a plain Rust allocation: the API
    // fills it and we free it by dropping. There is deliberately no
    // `FreeMibTable` here, because that would free a pointer into the middle of
    // this buffer, which is not an allocation at all.
    unsafe {
        let rc = GetAdaptersAddresses(AF_UNSPEC.0 as u32, FLAGS, None, None, &mut size);
        if rc != ERROR_BUFFER_OVERFLOW.0 || size == 0 {
            return None;
        }

        let mut buf = vec![0u8; size as usize];
        let mut rc = fill(&mut buf, &mut size);
        if rc == ERROR_BUFFER_OVERFLOW.0 && size as usize > buf.len() {
            // An adapter arrived between the two calls. One retry at the larger
            // size, then give up rather than chase a target that is moving
            // because the machine is busy — which is also when this poll is
            // least important.
            buf = vec![0u8; size as usize];
            rc = fill(&mut buf, &mut size);
        }
        if rc != ERROR_SUCCESS.0 {
            return None;
        }

        let mut rows: Vec<Row> = Vec::new();
        let mut node = buf.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        while let Some(row) = node.as_ref() {
            rows.push(Row {
                node,
                if_index: row.Anonymous1.Anonymous.IfIndex,
                if_type: row.IfType,
                up: row.OperStatus == IfOperStatusUp,
            });
            node = row.Next;
        }

        let node = rows.get(choose_row(&rows, want)?)?.node;
        // SAFETY: `node` points into `buf`, which is alive until this function
        // returns, and the API keeps every interior pointer in the table valid
        // for as long as the table itself.
        let row = &*node;
        Some(AdapterSample {
            name: pwstr_string(row.FriendlyName),
            description: pwstr_string(row.Description),
            local_addr: first_v4(row.FirstUnicastAddress),
            dns: dns_servers(row.FirstDnsServerAddress),
        })
    }
}

/// Adapter name, address and resolvers.
///
/// Stateless: `poll()` re-reads the table every time, so a network change shows
/// up on the next tick without anything here having to notice it happened.
#[derive(Default)]
pub struct Adapter;

impl Adapter {
    pub fn new() -> Self {
        Self
    }

    /// Detail for the adapter carrying the traffic, or `None` when there is no
    /// usable adapter — no interfaces, a stack with only loopback, or an API
    /// failure. Every field is independently optional, so a half-answered
    /// machine degrades one row at a time instead of losing the lot.
    pub fn poll(&mut self) -> Option<AdapterSample> {
        read()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::taskbar::format_addr;
    use windows::Win32::NetworkManagement::IpHelper::{IF_TYPE_ETHERNET_CSMACD, IF_TYPE_IEEE80211};

    /// A row the rule can judge without a live table.
    fn row(if_index: u32, if_type: u32, up: bool) -> Row {
        Row {
            node: std::ptr::null(),
            if_index,
            if_type,
            up,
        }
    }

    /// An address record pointing at a `sockaddr_in`, the way the API fills one.
    ///
    /// Boxed so the `SOCKET_ADDRESS` keeps pointing at the same bytes when the
    /// pair is returned; a bare local would be copied out and the pointer would
    /// follow the copy, not the original.
    fn record(
        family: windows::Win32::Networking::WinSock::ADDRESS_FAMILY,
        addr: u32,
    ) -> (Box<SOCKADDR_IN>, SOCKET_ADDRESS) {
        let mut sa = Box::new(SOCKADDR_IN {
            sin_family: family,
            ..Default::default()
        });
        sa.sin_addr.S_un.S_addr = addr;
        let socket = SOCKET_ADDRESS {
            lpSockaddr: (&raw mut *sa).cast(),
            iSockaddrLength: size_of::<SOCKADDR_IN>() as i32,
        };
        (sa, socket)
    }

    /// The exact `S_addr` this machine's stack reports for the address
    /// `ipconfig` prints as `192.168.1.1` — the same value the gateway test
    /// pins. If the direction is flipped, an adapter address reads
    /// `1.1.168.192`: plausible-looking, and wrong in a way nobody notices
    /// until they try to use it.
    #[test]
    fn ipv4_records_are_formatted_in_network_byte_order() {
        let (_sa, socket) = record(AF_INET, 0x0101_a8c0);
        // SAFETY: `socket` points at the boxed `sa` returned alongside it.
        let addr = unsafe { v4_of(&socket) };
        assert_eq!(addr.map(format_addr).as_deref(), Some("192.168.1.1"));
        // And the unformatted value really is the reversed reading, which is
        // what a `Ipv4Addr::from(u32)` would print.
        assert_ne!(addr, Some(u32::from_be_bytes([192, 168, 1, 1])));
    }

    #[test]
    fn a_second_ipv4_is_still_a_plain_value() {
        let (_sa, socket) = record(AF_INET, 0x0808_0808);
        // SAFETY: as above.
        let addr = unsafe { v4_of(&socket) };
        assert_eq!(addr.map(format_addr).as_deref(), Some("8.8.8.8"));
    }

    #[test]
    fn only_the_ipv4_family_is_read() {
        use windows::Win32::Networking::WinSock::AF_INET6;
        // An IPv6 record is a longer sockaddr; reading its first four bytes as
        // an IPv4 would print a link-local as a 0.x.y.z address.
        let (_sa, socket) = record(AF_INET6, 0x0101_a8c0);
        // SAFETY: as above.
        assert_eq!(unsafe { v4_of(&socket) }, None);
    }

    #[test]
    fn a_null_or_truncated_record_is_refused() {
        let empty = SOCKET_ADDRESS {
            lpSockaddr: std::ptr::null_mut(),
            iSockaddrLength: 0,
        };
        // SAFETY: nothing is dereferenced on either path.
        assert_eq!(unsafe { v4_of(&empty) }, None);

        let (_sa, mut short) = record(AF_INET, 0x0101_a8c0);
        short.iSockaddrLength = (size_of::<SOCKADDR_IN>() - 1) as i32;
        // SAFETY: the length check refuses this before any read.
        assert_eq!(unsafe { v4_of(&short) }, None);
    }

    #[test]
    fn the_routed_interface_is_the_row_reported() {
        let rows = [
            row(1, IF_TYPE_SOFTWARE_LOOPBACK, true),
            row(4, IF_TYPE_IEEE80211, true),
            row(9, IF_TYPE_ETHERNET_CSMACD, true),
        ];
        assert_eq!(choose_row(&rows, Some(9)), Some(2));
        assert_eq!(choose_row(&rows, Some(4)), Some(1));
    }

    #[test]
    fn a_stale_routed_index_falls_back_to_a_physical_adapter() {
        // The route index can name a row that has since gone down — a VPN that
        // dropped, a cable pulled — and reporting a down interface's address
        // would be worse than reporting the one that is still moving packets.
        let rows = [
            row(1, IF_TYPE_SOFTWARE_LOOPBACK, true),
            row(9, IF_TYPE_ETHERNET_CSMACD, true),
            row(12, IF_TYPE_IEEE80211, false),
        ];
        assert_eq!(choose_row(&rows, Some(12)), Some(1));
        assert_eq!(choose_row(&rows, Some(404)), Some(1));
        assert_eq!(choose_row(&rows, None), Some(1));
    }

    #[test]
    fn the_fallback_refuses_a_tunnel_but_the_route_does_not() {
        // A tunnel is up and is often where the traffic genuinely goes, so the
        // route wins when Windows says it carries the packet...
        let rows = [
            row(3, IF_TYPE_TUNNEL, true),
            row(9, IF_TYPE_ETHERNET_CSMACD, true),
        ];
        assert_eq!(choose_row(&rows, Some(3)), Some(0));
        // ...but it is the last thing to guess at when the route is unknown,
        // since its own address is a stub nobody recognises.
        assert_eq!(choose_row(&rows, None), Some(1));
    }

    #[test]
    fn a_machine_with_no_usable_adapter_has_no_row() {
        let rows = [
            row(1, IF_TYPE_SOFTWARE_LOOPBACK, true),
            row(3, IF_TYPE_TUNNEL, true),
        ];
        assert_eq!(choose_row(&rows, None), None);
        assert_eq!(choose_row(&[], Some(9)), None);
        assert_eq!(choose_row(&rows, Some(3)), Some(1), "routed beats the tunnel rule");
        assert_eq!(choose_row(&[], None), None);
    }
}
