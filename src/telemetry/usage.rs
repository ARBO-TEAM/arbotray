//! Rolling daily traffic totals — "how much of my data plan have I burned".
//!
//! Fed the *cumulative* interface counters rather than the per-second rate, so
//! the total is exact: summing a stream of rounded rates would drift by up to
//! half a byte per tick, every tick, forever.
//!
//! Persisted as one small JSON file rather than a database. A day's total is
//! two integers, and the retention window is one day — anything with a schema
//! and a migration story would be more machinery than the data deserves.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::System::SystemInformation::GetLocalTime;

/// How often the running total reaches disk. At the default 1 Hz poll, saving
/// every tick would be a disk write per second for a number nobody watches that
/// closely; the price is that a hard kill (or a power cut) loses at most this
/// much traffic, which does not matter for a quota readout.
const SAVE_EVERY: Duration = Duration::from_secs(30);

/// Bytes in a "G", matching what `format_rate` means by the same letter.
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// The on-disk shape.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Stored {
    /// Local date the totals belong to, `YYYY-MM-DD`.
    day: String,
    rx: u64,
    tx: u64,
}

/// Today's byte counters for the machine as a whole.
pub struct Usage {
    path: PathBuf,
    stored: Stored,
    /// The raw counters as of the previous poll. `None` until the first one,
    /// which is what stops the first tick from counting all traffic since boot.
    last: Option<(u64, u64)>,
    saved_at: Option<Instant>,
}

impl Usage {
    /// `%APPDATA%\ArboTray\usage.json`. Missing or corrupt history starts the
    /// day at zero rather than failing — this is a display counter, not a
    /// ledger, and refusing to start over it would be absurd.
    pub fn load() -> Self {
        let path = Self::path();
        let stored = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Stored>(&text).ok())
            .unwrap_or_default();
        let mut usage = Self {
            path,
            stored,
            last: None,
            saved_at: None,
        };
        // A file from an earlier day starts the new day at zero immediately,
        // so a process left running across midnight does not credit yesterday's
        // traffic to today.
        usage.roll_over();
        usage
    }

    pub fn path() -> PathBuf {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        base.join("ArboTray").join("usage.json")
    }

    /// Add the deltas between this cumulative counter reading and the previous
    /// one. A reading that went *backwards* is an adapter reset, not negative
    /// traffic — the delta is genuinely unknown, so it is dropped.
    pub fn record(&mut self, rx_total: u64, tx_total: u64) {
        self.roll_over();
        if let Some((prev_rx, prev_tx)) = self.last {
            if rx_total >= prev_rx {
                self.stored.rx = self.stored.rx.saturating_add(rx_total - prev_rx);
            }
            if tx_total >= prev_tx {
                self.stored.tx = self.stored.tx.saturating_add(tx_total - prev_tx);
            }
        }
        self.last = Some((rx_total, tx_total));
    }

    /// Write the total out, at most once per [`SAVE_EVERY`]. Failures are
    /// ignored: a read-only profile costs us the history, not the app.
    pub fn flush_if_due(&mut self) {
        let due = match self.saved_at {
            None => true,
            Some(at) => at.elapsed() >= SAVE_EVERY,
        };
        if !due {
            return;
        }
        self.saved_at = Some(Instant::now());
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string(&self.stored) {
            let _ = std::fs::write(&self.path, text);
        }
    }

    /// Zero the counters when the local date has moved on.
    fn roll_over(&mut self) {
        let today = today_key();
        if self.stored.day != today {
            self.stored = Stored {
                day: today,
                rx: 0,
                tx: 0,
            };
            // `saved_at` is left alone so the new day's zero is written on the
            // next due tick rather than immediately.
            self.saved_at = None;
        }
    }

    pub fn rx_bytes(&self) -> u64 {
        self.stored.rx
    }

    pub fn tx_bytes(&self) -> u64 {
        self.stored.tx
    }

    pub fn total_bytes(&self) -> u64 {
        self.stored.rx.saturating_add(self.stored.tx)
    }

    /// Today's total as one human-sized token, e.g. `1.4G`.
    pub fn text(&self) -> String {
        format_size(self.total_bytes())
    }

    /// Share of `quota_gb` consumed, 0..=∞. Deliberately *not* clamped at 100:
    /// the whole point of a quota readout is showing that you went over.
    /// `None` when no usable quota is configured.
    pub fn quota_pct(&self, quota_gb: f64) -> Option<f32> {
        if quota_gb <= 0.0 {
            return None;
        }
        Some((self.total_bytes() as f64 / (quota_gb * GIB) * 100.0) as f32)
    }
}

/// Bytes as one token with a unit — the same scale `format_rate` uses, minus
/// the per-second suffix.
pub fn format_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KIB {
        format!("{bytes}B")
    } else if b < KIB * KIB {
        format!("{:.1}K", b / KIB)
    } else if b < KIB * KIB * KIB {
        format!("{:.1}M", b / (KIB * KIB))
    } else {
        format!("{:.2}G", b / (KIB * KIB * KIB))
    }
}

/// `YYYY-MM-DD` in local time.
fn today_key() -> String {
    // SAFETY: `GetLocalTime` fills a struct we own; it cannot fail.
    let t: SYSTEMTIME = unsafe { GetLocalTime() };
    date_key(&t)
}

fn date_key(t: &SYSTEMTIME) -> String {
    format!("{:04}-{:02}-{:02}", t.wYear, t.wMonth, t.wDay)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: u16, m: u16, d: u16) -> SYSTEMTIME {
        SYSTEMTIME {
            wYear: y,
            wMonth: m,
            wDay: d,
            ..Default::default()
        }
    }

    /// A `Usage` that never touches disk.
    fn scratch(day: &str) -> Usage {
        Usage {
            path: std::env::temp_dir().join("arbotray-usage-test.json"),
            stored: Stored {
                day: day.into(),
                rx: 0,
                tx: 0,
            },
            last: None,
            saved_at: None,
        }
    }

    #[test]
    fn the_first_reading_is_a_baseline_not_traffic() {
        let mut u = scratch(&today_key());
        // Interface counters since boot are huge; counting them as today's
        // usage would report gigabytes the moment the app starts.
        u.record(9_000_000_000, 4_000_000_000);
        assert_eq!(u.total_bytes(), 0);
        u.record(9_000_001_000, 4_000_000_500);
        assert_eq!(u.rx_bytes(), 1_000);
        assert_eq!(u.tx_bytes(), 500);
    }

    #[test]
    fn deltas_accumulate_across_ticks() {
        let mut u = scratch(&today_key());
        u.record(100, 100);
        u.record(200, 300);
        u.record(400, 700);
        assert_eq!(u.rx_bytes(), 300);
        assert_eq!(u.tx_bytes(), 600);
        assert_eq!(u.total_bytes(), 900);
    }

    #[test]
    fn a_counter_reset_drops_that_delta_rather_than_underflowing() {
        let mut u = scratch(&today_key());
        u.record(5_000, 5_000);
        u.record(1_000, 900); // adapter replugged
        assert_eq!(u.total_bytes(), 0, "a reset is unknown, not negative");
        // The baseline moved, so the next real delta counts normally.
        u.record(1_500, 1_100);
        assert_eq!(u.rx_bytes(), 500);
        assert_eq!(u.tx_bytes(), 200);
    }

    #[test]
    fn a_stale_date_starts_the_new_day_at_zero() {
        let mut u = scratch("2000-01-01");
        u.stored.rx = 123_456;
        u.stored.tx = 654_321;
        u.record(10, 10);
        assert_eq!(u.total_bytes(), 0, "yesterday's traffic is not today's");
        assert_eq!(u.stored.day, today_key());
    }

    #[test]
    fn quota_reports_overage_instead_of_clamping() {
        let mut u = scratch(&today_key());
        u.stored.rx = 2 * 1024 * 1024 * 1024; // 2 GiB
        let pct = u.quota_pct(1.0).unwrap();
        assert!((pct - 200.0).abs() < 0.01, "expected 200%, got {pct}");
        assert_eq!(u.quota_pct(2.0), Some(pct / 2.0));
    }

    #[test]
    fn a_useless_quota_yields_no_percentage() {
        let u = scratch(&today_key());
        assert_eq!(u.quota_pct(0.0), None);
        assert_eq!(u.quota_pct(-1.0), None);
    }

    #[test]
    fn sizes_pick_a_sane_unit() {
        assert_eq!(format_size(0), "0B");
        assert_eq!(format_size(999), "999B");
        assert_eq!(format_size(2048), "2.0K");
        assert_eq!(format_size(5 * 1024 * 1024), "5.0M");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024), "3.00G");
    }

    #[test]
    fn date_keys_are_zero_padded_and_sort() {
        assert_eq!(date_key(&at(2026, 9, 6)), "2026-09-06");
        assert_eq!(date_key(&at(2026, 12, 31)), "2026-12-31");
        // Lexicographic order must match chronological order, since that is
        // the only comparison the rollover makes.
        assert!(date_key(&at(2026, 9, 9)) < date_key(&at(2026, 9, 10)));
        assert!(date_key(&at(2026, 9, 30)) < date_key(&at(2026, 10, 1)));
    }
}
