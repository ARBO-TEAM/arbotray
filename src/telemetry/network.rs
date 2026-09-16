//! Throughput collector: sums the IP Helper octet counters over every live
//! interface and reports the delta between polls, in bytes/sec.

use super::NetSample;
use std::time::Instant;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetIfTable2, IF_TYPE_SOFTWARE_LOOPBACK, MIB_IF_TABLE2,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;

/// Shortest window we will divide by. Below it a stray double-poll would read
/// the counters microseconds apart and report a meaningless spike.
const MIN_ELAPSED_SECS: f64 = 0.1;

/// Bytes/sec from one counter pair over `elapsed_secs`.
///
/// A counter that went *backwards* means the interface was reset (adapter
/// replugged, driver reloaded) — that is a lost delta, not a negative one, so
/// it reports 0 rather than wrapping into a huge spike.
fn rate(prev: u64, now: u64, elapsed_secs: f64) -> u64 {
    if elapsed_secs < MIN_ELAPSED_SECS || now < prev {
        return 0;
    }
    ((now - prev) as f64 / elapsed_secs) as u64
}

/// `(rx, tx)` octets summed over interfaces that are up and not loopback.
///
/// `None` only when IP Helper itself fails — a machine with no interfaces at
/// all yields `Some((0, 0))`.
fn read_counters() -> Option<(u64, u64)> {
    // SAFETY: `table` is written by GetIfTable2 and stays owned by us until
    // FreeMibTable; the row slice is bounded by NumEntries, which the API
    // guarantees matches the allocation.
    unsafe {
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        if GetIfTable2(&mut table) != ERROR_SUCCESS || table.is_null() {
            return None;
        }
        let rows =
            std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize);
        let (mut rx, mut tx) = (0u64, 0u64);
        for row in rows {
            if row.OperStatus != IfOperStatusUp || row.Type == IF_TYPE_SOFTWARE_LOOPBACK {
                continue;
            }
            rx = rx.saturating_add(row.InOctets);
            tx = tx.saturating_add(row.OutOctets);
        }
        FreeMibTable(table.cast());
        Some((rx, tx))
    }
}

/// Sample of total network throughput.
#[derive(Default)]
pub struct Network {
    /// Last `(rx, tx, when)`. `None` until the first successful poll, which is
    /// what makes the first sample read as 0 rather than as one second of
    /// traffic since boot.
    prev: Option<(u64, u64, Instant)>,
}

impl Network {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bytes/sec since the previous poll. The first call returns `Some(0, 0)`
    /// because it has no baseline to subtract.
    pub fn poll(&mut self) -> Option<NetSample> {
        let (rx, tx) = read_counters()?;
        let now = Instant::now();

        let sample = match self.prev {
            None => NetSample::default(),
            Some((prev_rx, prev_tx, at)) => {
                let elapsed = now.duration_since(at).as_secs_f64();
                NetSample {
                    rx_bps: rate(prev_rx, rx, elapsed),
                    tx_bps: rate(prev_tx, tx, elapsed),
                }
            }
        };

        self.prev = Some((rx, tx, now));
        Some(sample)
    }

    /// The most recent cumulative `(rx, tx)` octets, before any rate division.
    ///
    /// Quota accounting diffs these directly — a byte total built by summing
    /// rounded per-second rates drifts, one truncation at a time.
    pub fn totals(&self) -> Option<(u64, u64)> {
        self.prev.map(|(rx, tx, _)| (rx, tx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steady_traffic_divides_by_elapsed() {
        assert_eq!(rate(1_000, 3_000, 2.0), 1_000);
        assert_eq!(rate(0, 1_500, 1.0), 1_500);
    }

    #[test]
    fn counter_reset_is_a_zero_delta_not_a_spike() {
        // Adapter reset: counters fall back to near zero.
        assert_eq!(rate(9_000_000_000, 42, 1.0), 0);
        assert_eq!(rate(1, 0, 1.0), 0);
    }

    #[test]
    fn equal_counters_are_zero() {
        assert_eq!(rate(500, 500, 1.0), 0);
    }

    #[test]
    fn implausibly_short_window_is_refused() {
        assert_eq!(rate(0, 1_000_000, 0.0), 0);
        assert_eq!(rate(0, 1_000_000, MIN_ELAPSED_SECS / 2.0), 0);
        // Exactly at the floor we do divide.
        assert_eq!(rate(0, 1_000, MIN_ELAPSED_SECS), 10_000);
    }

    #[test]
    fn fractional_elapsed_rounds_down() {
        assert_eq!(rate(0, 1_000, 3.0), 333);
    }
}
