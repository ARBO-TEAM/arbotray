//! Ping collector: round-trip to the default gateway, plus loss over a short
//! rolling window.
//!
//! `IcmpSendEcho` is synchronous, so this is the one collector that can stall
//! the tray. Two things keep that bounded: a tight per-probe timeout, and a
//! re-probe interval — the tray polls at 1 Hz, we ping every [`PROBE_INTERVAL`]
//! and hand back the cached sample in between.

use super::LatencySample;
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::NetworkManagement::IpHelper::{
    GetBestRoute2, IcmpCloseHandle, IcmpCreateFile, IcmpSendEcho, ICMP_ECHO_REPLY, IP_OPTION_INFORMATION,
    IP_SUCCESS, MIB_IPFORWARD_ROW2,
};
use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN, SOCKADDR_INET};

/// How often we actually put packets on the wire.
const PROBE_INTERVAL: Duration = Duration::from_secs(3);

/// Per-probe reply timeout. Stays well inside the ~100ms spirit of the contract
/// because the *caller* only pays it once every [`PROBE_INTERVAL`]; a losing
/// probe costs one stalled poll, not one per second.
const PROBE_TIMEOUT_MS: u32 = 1000;

/// Per-probe reply timeout for the *second* ping in a poll. It is additive to
/// [`PROBE_TIMEOUT_MS`] rather than competing with it, so a dead WAN costs a
/// quarter second instead of doubling the worst-case stall. Ample for a reply
/// that normally lands in tens of milliseconds.
const INTERNET_TIMEOUT_MS: u32 = 250;

/// Probes kept for the loss percentage.
const WINDOW: usize = 10;

/// 8.8.8.8, as a host-order `u32` literal (`u32::from_be_bytes` of the octets).
const INTERNET_TARGET: u32 = u32::from_be_bytes([8, 8, 8, 8]);

/// Loss over a window of probe outcomes. `None` until at least one probe has
/// been recorded — a percentage of nothing is not 0%, it is unknown.
fn loss_pct(window: &VecDeque<bool>) -> Option<u32> {
    if window.is_empty() {
        return None;
    }
    let lost = window.iter().filter(|hit| !**hit).count();
    Some((lost * 100 / window.len()) as u32)
}

/// Push one outcome, evicting the oldest so the window stays bounded.
fn record(window: &mut VecDeque<bool>, hit: bool) {
    if window.len() == WINDOW {
        window.pop_front();
    }
    window.push_back(hit);
}

/// Default gateway as a host-order IPv4 address, or `None` on an IPv6-only or
/// unconfigured stack.
fn default_gateway() -> Option<u32> {
    // A zeroed destination prefix with an all-zero next hop asks "where would a
    // packet to 0.0.0.0 go?" — the API answers with the default route.
    let dest = SOCKADDR_INET {
        Ipv4: SOCKADDR_IN {
            sin_family: AF_INET,
            ..Default::default()
        },
    };
    let mut route = MIB_IPFORWARD_ROW2::default();
    let mut source = SOCKADDR_INET::default();

    // SAFETY: out-params are live locals; `dest` outlives the call.
    let rc = unsafe {
        GetBestRoute2(
            None,
            0,
            None,
            &dest,
            0,
            &mut route,
            &mut source,
        )
    };
    if rc != ERROR_SUCCESS {
        return None;
    }
    // SAFETY: GetBestRoute2 fills NextHop as an AF_INET sockaddr for an IPv4
    // default route; reading the Ipv4 arm is only valid for that family.
    unsafe {
        if route.NextHop.si_family == AF_INET {
            let addr = route.NextHop.Ipv4.sin_addr.S_un.S_addr;
            (addr != 0).then_some(addr)
        } else {
            None
        }
    }
}

/// One ICMP echo. `Some(ms)` on a reply, `None` on timeout, error or no route.
fn ping(
    handle: windows::Win32::Foundation::HANDLE,
    target: u32,
    timeout_ms: u32,
) -> Option<u32> {
    // 32 bytes of payload plus room for the reply header, as documented.
    let payload = [0u8; 32];
    let mut reply = [0u8; std::mem::size_of::<ICMP_ECHO_REPLY>() + 32 + 8];

    // SAFETY: `reply` is a live buffer large enough for one reply record plus
    // the echoed payload; the API writes at most that and returns the count.
    let replied = unsafe {
        IcmpSendEcho(
            handle,
            target,
            payload.as_ptr().cast(),
            payload.len() as u16,
            None::<*const IP_OPTION_INFORMATION>,
            reply.as_mut_ptr().cast(),
            reply.len() as u32,
            timeout_ms,
        )
    };
    if replied == 0 {
        return None;
    }
    // SAFETY: the buffer is at least one ICMP_ECHO_REPLY and the API reported a
    // reply, so the header is initialised.
    unsafe {
        let header = &*(reply.as_ptr() as *const ICMP_ECHO_REPLY);
        (header.Status == IP_SUCCESS).then_some(header.RoundTripTime)
    }
}

/// Gateway reachability and probe loss.
#[derive(Default)]
pub struct Latency {
    /// Live `IcmpCreateFile` handle. `None` if ICMP could not be set up at all.
    handle: Option<windows::Win32::Foundation::HANDLE>,
    window: VecDeque<bool>,
    /// Last completed sample and when it was taken, so polls between probes are
    /// answered from cache instead of blocking.
    cached: Option<(LatencySample, Instant)>,
    next_probe: Option<Instant>,
}

impl Drop for Latency {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            // SAFETY: the handle came from IcmpCreateFile and is closed once.
            unsafe {
                let _ = IcmpCloseHandle(handle);
            }
        }
    }
}

impl Latency {
    pub fn new() -> Self {
        // SAFETY: no arguments; a failure just means no ICMP handle.
        let handle = unsafe { IcmpCreateFile().ok() };
        Self {
            handle,
            window: VecDeque::new(),
            cached: None,
            next_probe: None,
        }
    }

    /// Cached gateway latency and loss. The first call always probes; later
    /// calls probe only once per [`PROBE_INTERVAL`].
    pub fn poll(&mut self) -> Option<LatencySample> {
        let now = Instant::now();
        if let (Some(cached), Some(next)) = (&self.cached, self.next_probe) {
            if now < next {
                // `.0` is the sample; `.1` is when it was taken.
                return Some(cached.0);
            }
        }
        self.next_probe = Some(now + PROBE_INTERVAL);

        let handle = self.handle?;
        let Some(gateway) = default_gateway() else {
            self.record_outcome(false);
            let sample = self.sample(None, None, None);
            self.cached = Some((sample, now));
            return Some(sample);
        };

        let gateway_ms = ping(handle, gateway, PROBE_TIMEOUT_MS);
        self.record_outcome(gateway_ms.is_some());

        // Both are reported rather than one standing in for the other: an
        // unreachable gateway with no internet is a local fault, while a
        // healthy gateway with no internet is the provider's — and the
        // difference is the only reason anyone opens this page.
        let internet_ms = ping(handle, INTERNET_TARGET, INTERNET_TIMEOUT_MS);

        let sample = self.sample(Some(gateway), gateway_ms, internet_ms);
        self.cached = Some((sample, now));
        Some(sample)
    }

    fn record_outcome(&mut self, hit: bool) {
        record(&mut self.window, hit);
    }

    fn sample(
        &self,
        gateway_addr: Option<u32>,
        gateway_ms: Option<u32>,
        internet_ms: Option<u32>,
    ) -> LatencySample {
        LatencySample {
            gateway_addr,
            gateway_ms,
            internet_ms,
            loss_pct: loss_pct(&self.window),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loss_is_none_before_any_probe() {
        assert_eq!(loss_pct(&VecDeque::new()), None);
    }

    #[test]
    fn loss_counts_misses() {
        let mut w = VecDeque::new();
        for hit in [true, true, false, true] {
            record(&mut w, hit);
        }
        assert_eq!(loss_pct(&w), Some(25));
    }

    #[test]
    fn all_misses_is_one_hundred_percent() {
        let mut w = VecDeque::new();
        for _ in 0..4 {
            record(&mut w, false);
        }
        assert_eq!(loss_pct(&w), Some(100));
    }

    #[test]
    fn no_misses_is_zero_percent() {
        let mut w = VecDeque::new();
        record(&mut w, true);
        assert_eq!(loss_pct(&w), Some(0));
    }

    #[test]
    fn window_is_bounded_and_evicts_oldest() {
        let mut w = VecDeque::new();
        // Ten misses, then ten hits: the misses must fall out of the window.
        for _ in 0..WINDOW {
            record(&mut w, false);
        }
        assert_eq!(loss_pct(&w), Some(100));
        for _ in 0..WINDOW {
            record(&mut w, true);
        }
        assert_eq!(w.len(), WINDOW);
        assert_eq!(loss_pct(&w), Some(0));
    }

    #[test]
    fn window_truncates_rather_than_rounds() {
        let mut w = VecDeque::new();
        // 1 loss in 3 is 33.3%, which truncates to 33.
        for hit in [false, true, true] {
            record(&mut w, hit);
        }
        assert_eq!(loss_pct(&w), Some(33));
    }
}
