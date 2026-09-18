//! Throughput collector: sums the IP Helper octet counters over the live
//! hardware interfaces and reports the delta between polls, in bytes/sec.

use super::NetSample;
use std::time::Instant;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;

/// Shortest window we will divide by. Below it a stray double-poll would read
/// the counters microseconds apart and report a meaningless spike.
const MIN_ELAPSED_SECS: f64 = 0.1;

/// The counters of one interface-table row, as a plain value.
///
/// Split out from the API call so the rule deciding which rows count can be
/// tested without a live interface table — which is the whole point, because
/// getting it wrong is invisible on the tray and wrong by a factor of four.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Row {
    up: bool,
    /// `MIB_IF_ROW2::InterfaceAndOperStatusFlags.HardwareInterface`. False for
    /// the filter-driver and virtual rows, which is what makes this the field
    /// that matters.
    hardware: bool,
    rx: u64,
    tx: u64,
}

/// Whether a row's octets belong to the machine's real traffic.
///
/// Only the hardware interface counts. Every protocol driver bound to a NIC —
/// WFP, QoS Packet Scheduler, the Hyper-V switch extension — gets its own row
/// in the IP Helper table, and each of those rows reports its *parent NIC's
/// counters verbatim* rather than its own. An Ethernet adapter with three
/// filters bound therefore appears four times with identical numbers, so summing
/// every up row counts the same bytes four times over: a machine with 7 GB of
/// real traffic since boot reports 28 GB, and every speed reading is 4× high.
///
/// Virtual adapters (Hyper-V `vEthernet`, VPN tunnels) are left out for the same
/// reason they are unnecessary: bytes crossing one of those cross the physical
/// NIC too, so counting the filter rows as well would be double counting by a
/// narrower margin. What a data plan is burned by is what the hardware carried.
///
/// There is deliberately no loopback test. `Loopback Pseudo-Interface` is not a
/// hardware interface, so the bit above already excludes it — verified against
/// a live table, where it reports `type=24, up=1, hw=0`. A separate check on
/// `IF_TYPE_SOFTWARE_LOOPBACK` would be dead code that reads like a safeguard.
fn counts(row: &Row) -> bool {
    row.up && row.hardware
}

/// `(rx, tx)` octets summed over the rows that count.
fn sum(rows: &[Row]) -> (u64, u64) {
    let (mut rx, mut tx) = (0u64, 0u64);
    for row in rows.iter().filter(|r| counts(r)) {
        rx = rx.saturating_add(row.rx);
        tx = tx.saturating_add(row.tx);
    }
    (rx, tx)
}

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

/// `(rx, tx)` octets summed over the rows [`counts`] accepts.
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
        let api_rows =
            std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize);
        let rows: Vec<Row> = api_rows
            .iter()
            .map(|row| Row {
                up: row.OperStatus == IfOperStatusUp,
                // The binding exposes `MIB_IF_ROW2`'s flags union as a raw
                // byte; bit 0 is `HardwareInterface`.
                hardware: row.InterfaceAndOperStatusFlags._bitfield & 1 != 0,
                rx: row.InOctets,
                tx: row.OutOctets,
            })
            .collect();
        FreeMibTable(table.cast());
        Some(sum(&rows))
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

    /// A row that counts, for tests that do not care about the flags.
    fn live(rx: u64, tx: u64) -> Row {
        Row {
            up: true,
            hardware: true,
            rx,
            tx,
        }
    }

    /// The real row set from a machine whose Ethernet adapter has three
    /// protocol drivers bound, reduced to the fields that decide the sum. This
    /// is the shape that produced a 13 GB day total from under 7 GB of traffic:
    /// the hardware row plus three filter rows, all four reporting Ethernet's
    /// counters.
    #[test]
    fn filter_driver_rows_are_not_counted_alongside_their_nic() {
        let ethernet = (2_096_120_354, 4_918_213_465);
        let rows = [
            Row {
                up: true,
                hardware: false,
                rx: 0,
                tx: 0,
            }, // vSwitch, idle
            Row {
                up: true,
                hardware: false,
                rx: ethernet.0,
                tx: ethernet.1,
            }, // WFP
            Row {
                up: true,
                hardware: false,
                rx: ethernet.0,
                tx: ethernet.1,
            }, // QoS
            Row {
                up: true,
                hardware: false,
                rx: ethernet.0,
                tx: ethernet.1,
            }, // WFP 802.3
            Row {
                up: true,
                hardware: true,
                rx: ethernet.0,
                tx: ethernet.1,
            }, // the NIC itself
        ];
        assert_eq!(sum(&rows), ethernet, "the NIC's bytes are counted once");
        // And the sum that was taken before this rule, for the record: four
        // copies, 28 GB against the machine's real 7 GB.
        let naive: u64 = rows.iter().map(|r| r.rx + r.tx).sum();
        assert_eq!(naive, ethernet.0 * 4 + ethernet.1 * 4);
    }

    #[test]
    fn a_down_or_virtual_row_is_left_out() {
        let mut down = live(1_000, 1_000);
        down.up = false;
        let mut virtual_adapter = live(1_000, 1_000);
        virtual_adapter.hardware = false;
        assert_eq!(sum(&[down]), (0, 0));
        assert_eq!(sum(&[virtual_adapter]), (0, 0), "Hyper-V vEthernet, tunnels");
        assert_eq!(sum(&[]), (0, 0));
    }

    #[test]
    fn two_physical_nics_add_up() {
        // Ethernet and Wi-Fi both up and both moving traffic is a real
        // configuration, and their counters are genuinely independent — unlike
        // a NIC and its filters.
        let rows = [live(100, 200), live(1_000, 2_000)];
        assert_eq!(sum(&rows), (1_100, 2_200));
    }
}
