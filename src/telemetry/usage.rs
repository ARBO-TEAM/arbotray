//! Rolling daily traffic totals — "how much of my data plan have I burned".
//!
//! Fed the *cumulative* interface counters rather than the per-second rate, so
//! the total is exact: summing a stream of rounded rates would drift by up to
//! half a byte per tick, every tick, forever.
//!
//! Persisted as one small JSON file rather than a database. A day's total is
//! two integers and the file keeps a week of them — anything with a schema and
//! a migration story would be more machinery than the data deserves.

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

/// The number of daily records kept when the config says nothing sane.
const MIN_KEEP_DAYS: usize = 1;
/// Ten years. Past any use, and it bounds what a hand-edited `raw_days` can
/// grow this file to.
const MAX_KEEP_DAYS: usize = 3650;

/// One day's byte counters.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Day {
    /// Local date, `YYYY-MM-DD`. Zero-padded, so lexicographic order is
    /// chronological and a month is a string prefix.
    day: String,
    rx: u64,
    tx: u64,
}

impl Day {
    fn total(&self) -> u64 {
        self.rx.saturating_add(self.tx)
    }
}

/// The on-disk shape: a rolling window of days, oldest first.
///
/// A *list* rather than one record, because a single record answers "is it me
/// or my provider?" and never "is this month unusual?" — the second question
/// needs yesterday to still be there when today is read.
///
/// A file from before this shape existed has no `days` key and loads as an
/// empty window, so the day in flight starts again at zero. One day's total,
/// once, on upgrade — a migration for a two-integer counter would be more
/// machinery than the lost data.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    days: Vec<Day>,
}

/// What the machine has moved, by day. The counters themselves are cumulative
/// since boot; nothing here counts a *rate*.
pub struct Usage {
    path: PathBuf,
    stored: Stored,
    /// The raw counters as of the previous poll. `None` until the first one,
    /// which is what stops the first tick from counting all traffic since boot.
    last: Option<(u64, u64)>,
    saved_at: Option<Instant>,
    /// How many days the file keeps, from `Retention.raw_days`. Bounded on the
    /// way in: `0` would erase today's total as it is written.
    keep_days: usize,
}

impl Usage {
    /// `%APPDATA%\ArboTray\usage.json`, keeping `keep_days` days. Missing or
    /// corrupt history starts the day at zero rather than failing — this is a
    /// display counter, not a ledger, and refusing to start over it would be
    /// absurd.
    pub fn load(keep_days: u32) -> Self {
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
            keep_days: clamp_keep_days(keep_days),
        };
        usage.prune();
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
        let Some(today) = self.stored.days.last_mut() else {
            // `roll_over` guarantees a record exists, but returning here beats
            // panicking on an index in a background thread.
            self.last = Some((rx_total, tx_total));
            return;
        };
        if let Some((prev_rx, prev_tx)) = self.last {
            if rx_total >= prev_rx {
                today.rx = today.rx.saturating_add(rx_total - prev_rx);
            }
            if tx_total >= prev_tx {
                today.tx = today.tx.saturating_add(tx_total - prev_tx);
            }
        }
        self.last = Some((rx_total, tx_total));
    }

    /// Write the history out, at most once per [`SAVE_EVERY`]. Failures are
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

    /// Start a new day's record when the local date has moved on. Yesterday's
    /// is *kept* — it is the whole point of the file — and the oldest falls off
    /// the front once the window is full.
    fn roll_over(&mut self) {
        let today = today_key();
        if self.stored.days.last().is_some_and(|d| d.day == today) {
            return;
        }
        self.stored.days.push(Day {
            day: today,
            rx: 0,
            tx: 0,
        });
        self.prune();
        // `saved_at` is cleared so the new day's zero reaches disk on the next
        // due tick rather than waiting out the current interval.
        self.saved_at = None;
    }

    /// Drop the oldest records past the retention window. The day in flight is
    /// never dropped: a window of zero would erase the total as it is written.
    fn prune(&mut self) {
        let excess = self.stored.days.len().saturating_sub(self.keep_days);
        if excess > 0 {
            self.stored.days.drain(..excess);
        }
    }

    /// Today's record. Always present after `load`, since `roll_over` appends
    /// one; `None` only for a `Usage` built by hand.
    fn today(&self) -> Option<&Day> {
        self.stored.days.last()
    }

    pub fn rx_bytes(&self) -> u64 {
        self.today().map_or(0, |d| d.rx)
    }

    pub fn tx_bytes(&self) -> u64 {
        self.today().map_or(0, |d| d.tx)
    }

    pub fn total_bytes(&self) -> u64 {
        self.today().map_or(0, Day::total)
    }

    /// Today's total as one human-sized token, e.g. `1.4G`.
    pub fn text(&self) -> String {
        format_size(self.total_bytes())
    }

    /// Today's date as one token, e.g. `09-16`.
    pub fn today_key(&self) -> String {
        short_key(self.today().map_or("", |d| d.day.as_str()))
    }

    /// Days kept, oldest first, as `(MM-DD, total)` — what the Data page draws.
    pub fn history(&self) -> Vec<(String, u64)> {
        self.stored
            .days
            .iter()
            .map(|d| (short_key(&d.day), d.total()))
            .collect()
    }

    /// Traffic since the first of the current month, in bytes.
    ///
    /// Only over the days the file still holds, so with a retention window
    /// shorter than a month this is a sum of the recent past and *not* a
    /// month-to-date reading. [`Usage::covers_whole_month`] is what says which.
    pub fn month_bytes(&self) -> u64 {
        let Some(month) = self.today().map(|d| short_key(&d.day)) else {
            return 0;
        };
        let month = &month[..2];
        self.stored
            .days
            .iter()
            .filter(|d| d.day.len() == 10 && d.day[5..7] == *month)
            .map(Day::total)
            .fold(0u64, u64::saturating_add)
    }

    /// Whether the oldest record kept is already inside the current month, in
    /// which case [`Usage::month_bytes`] is the real month-to-date total.
    /// Otherwise it is a partial sum and should say so.
    pub fn covers_whole_month(&self) -> bool {
        let (Some(first), Some(today)) = (self.stored.days.first(), self.today()) else {
            return false;
        };
        // `[5..7]` is the month of a `YYYY-MM-DD` key: the year is `[..4]`, so
        // comparing `[..2]` here would compare "20" against "09" and always be
        // false — a caveat the page would then show on every machine.
        first.day.len() == 10 && today.day.len() == 10 && first.day[5..7] == today.day[5..7]
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

/// Bound `Retention.raw_days` to something the file can hold.
pub fn clamp_keep_days(days: u32) -> usize {
    (days as usize).clamp(MIN_KEEP_DAYS, MAX_KEEP_DAYS)
}

/// `YYYY-MM-DD` to the `MM-DD` the Data page shows. The year is never dropped
/// when it is needed to tell two records apart — it simply never is, inside a
/// window of at most ten years.
fn short_key(day: &str) -> String {
    if day.len() == 10 {
        day[5..].to_string()
    } else {
        day.to_string()
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
                days: vec![Day {
                    day: day.into(),
                    rx: 0,
                    tx: 0,
                }],
            },
            last: None,
            saved_at: None,
            keep_days: clamp_keep_days(7),
        }
    }

    /// A `Usage` holding `days` in order, oldest first, each with the same
    /// total — for the retention and month-sum rules.
    fn with_days(days: &[&str], keep: u32) -> Usage {
        let mut u = scratch(days.last().copied().unwrap_or("2026-09-16"));
        u.stored.days = days
            .iter()
            .map(|d| Day {
                day: (*d).into(),
                rx: 500,
                tx: 500,
            })
            .collect();
        u.keep_days = clamp_keep_days(keep);
        u
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
    fn a_stale_date_starts_the_new_day_at_zero_and_keeps_the_old_one() {
        let mut u = scratch("2000-01-01");
        u.stored.days[0].rx = 123_456;
        u.stored.days[0].tx = 654_321;
        u.record(10, 10);
        assert_eq!(u.total_bytes(), 0, "yesterday's traffic is not today's");
        assert_eq!(u.stored.days.last().unwrap().day, today_key());
        // The old day is still there — that is the whole reason the file holds
        // a window of them rather than one record.
        assert_eq!(u.stored.days.len(), 2);
        assert_eq!(u.stored.days[0].day, "2000-01-01");
        assert_eq!(u.stored.days[0].total(), 123_456 + 654_321);
    }

    #[test]
    fn the_window_drops_its_oldest_day_and_never_the_day_in_flight() {
        let mut u = with_days(&["2026-09-12", "2026-09-13", "2026-09-14"], 3);
        // A fourth day arrives, so the oldest has to go.
        u.record(0, 0);
        assert_eq!(u.stored.days.len(), 3, "the window is three days");
        assert_eq!(u.stored.days.first().unwrap().day, "2026-09-13");
        assert_eq!(u.stored.days.last().unwrap().day, today_key());

        // A window of zero would erase today's total as it is written, so the
        // clamp holds one day rather than none.
        let mut z = with_days(&["2026-09-15"], 0);
        z.record(0, 0);
        assert_eq!(z.stored.days.len(), 1);
        assert_eq!(z.stored.days[0].day, today_key());
    }

    #[test]
    fn the_month_total_sums_this_month_and_says_when_it_cannot() {
        // Five days of the current month, all kept: a real month-to-date sum.
        let kept = with_days(
            &["2026-09-12", "2026-09-13", "2026-09-14", "2026-09-15", "2026-09-16"],
            30,
        );
        assert!(kept.covers_whole_month());
        assert_eq!(kept.month_bytes(), 5 * 1000);

        // Last month's days are in the window but not in this month's total.
        let straddling = with_days(&["2026-08-30", "2026-09-01", "2026-09-02"], 30);
        assert_eq!(
            straddling.month_bytes(),
            2000,
            "August must not be counted as September"
        );
        // The window starts last month, so this is a partial sum — the page has
        // to say so rather than reporting it as month-to-date.
        assert!(!straddling.covers_whole_month());
    }

    #[test]
    fn quota_reports_overage_instead_of_clamping() {
        let mut u = scratch(&today_key());
        u.stored.days[0].rx = 2 * 1024 * 1024 * 1024; // 2 GiB
        let pct = u.quota_pct(1.0).unwrap();
        assert!((pct - 200.0).abs() < 0.01, "expected 200%, got {pct}");
        assert_eq!(u.quota_pct(2.0), Some(pct / 2.0));
    }

    #[test]
    fn a_file_from_the_one_day_shape_still_loads() {
        // The riskiest part of the change to a window of days: an existing
        // install has a `usage.json` holding a single `{day, rx, tx}`. If that
        // shape failed to parse, `load` would fall back to defaults — silently
        // discarding the day in flight on a machine that never touched a config
        // *and* refusing nothing, so nothing would report it. Pinned here
        // because the loss is invisible at runtime.
        let old = r#"{"day":"2026-09-16","rx":1234,"tx":5678}"#;
        let parsed: Stored = serde_json::from_str(old).expect("the old shape must still parse");
        assert!(
            parsed.days.is_empty(),
            "an old file starts the window empty rather than failing to load"
        );

        // And the new shape round-trips, so the window survives a restart.
        let now = Stored {
            days: vec![Day {
                day: "2026-09-16".into(),
                rx: 1,
                tx: 2,
            }],
        };
        let text = serde_json::to_string(&now).unwrap();
        let back: Stored = serde_json::from_str(&text).unwrap();
        assert_eq!(back.days.len(), 1);
        assert_eq!(back.days[0].total(), 3);
    }

    #[test]
    fn history_is_short_keys_oldest_first() {
        // The Data page draws these directly, so the pairing of a label and its
        // total is the contract.
        let u = with_days(&["2026-09-14", "2026-09-15"], 7);
        assert_eq!(
            u.history(),
            vec![("09-14".to_string(), 1000), ("09-15".to_string(), 1000)]
        );
        // Oldest first, so the reader's eye moves forward in time.
        assert_eq!(u.today_key(), "09-15");
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
