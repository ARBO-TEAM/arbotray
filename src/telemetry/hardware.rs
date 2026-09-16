//! CPU and RAM collector.
//!
//! Deliberately built on `GetSystemTimes` + `GlobalMemoryStatusEx` rather than
//! PDH: two cheap calls, no query handles to open, close or leak, and the
//! numbers the tray shows are system-wide totals, which is exactly what these
//! give us.

use super::HardwareSample;
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::GetSystemTimes;

/// `(idle, kernel, user)` in 100ns ticks. `kernel` already includes `idle`.
type Ticks = (u64, u64, u64);

fn to_u64(ft: FILETIME) -> u64 {
    (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)
}

/// CPU busy percentage from two `GetSystemTimes` readings.
///
/// `total` is kernel+user and `kernel` contains the idle time, so busy is
/// `total - idle`. A zero-length or backwards window (no measurable tick
/// elapsed, or the counters were read concurrently) has no defined percentage
/// and reports 0.
fn cpu_pct_from_deltas(prev: Ticks, now: Ticks) -> f32 {
    let (p_idle, p_kernel, p_user) = prev;
    let (n_idle, n_kernel, n_user) = now;
    if n_idle < p_idle || n_kernel < p_kernel || n_user < p_user {
        return 0.0;
    }
    let idle = n_idle - p_idle;
    let total = (n_kernel - p_kernel) + (n_user - p_user);
    if total == 0 {
        return 0.0;
    }
    // `busy` cannot exceed `total` here, so the ratio is already 0..=100.
    let busy = total.saturating_sub(idle);
    (busy as f64 / total as f64 * 100.0) as f32
}

fn read_ticks() -> Option<Ticks> {
    let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: all three out-params point at live, correctly-typed locals.
    unsafe {
        GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)).ok()?;
    }
    Some((to_u64(idle), to_u64(kernel), to_u64(user)))
}

/// Sample of global CPU load and physical memory.
#[derive(Default)]
pub struct Hardware {
    prev: Option<Ticks>,
}

impl Hardware {
    pub fn new() -> Self {
        Self::default()
    }

    /// CPU, RAM used and RAM total. The first call only latches the CPU
    /// baseline and returns `None` — one tick with no CPU figure, then a real
    /// percentage from the second call on.
    pub fn poll(&mut self) -> Option<HardwareSample> {
        let ticks = read_ticks()?;

        let mut status = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        // SAFETY: `status.dwLength` is set to the struct size, as the API requires.
        unsafe {
            GlobalMemoryStatusEx(&mut status).ok()?;
        }

        let cpu_pct = match self.prev {
            None => {
                self.prev = Some(ticks);
                return None;
            }
            Some(prev) => cpu_pct_from_deltas(prev, ticks),
        };
        self.prev = Some(ticks);

        Some(HardwareSample {
            cpu_pct,
            ram_used_bytes: status.ullTotalPhys.saturating_sub(status.ullAvailPhys),
            ram_total_bytes: status.ullTotalPhys,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window of 1000 ticks where `idle` went to `idle_of_total`.
    fn window(idle: u64, total: u64) -> (Ticks, Ticks) {
        // kernel counts idle, user is the rest of the total.
        ((0, 0, 0), (idle, total, 0))
    }

    #[test]
    fn fully_idle_is_zero_percent() {
        let (prev, now) = window(1_000, 1_000);
        assert_eq!(cpu_pct_from_deltas(prev, now), 0.0);
    }

    #[test]
    fn fully_busy_is_one_hundred_percent() {
        let (prev, now) = window(0, 1_000);
        assert_eq!(cpu_pct_from_deltas(prev, now), 100.0);
    }

    #[test]
    fn three_quarter_idle_is_twenty_five_percent() {
        let (prev, now) = window(750, 1_000);
        assert_eq!(cpu_pct_from_deltas(prev, now), 25.0);
    }

    #[test]
    fn kernel_includes_idle_so_busy_is_total_minus_idle() {
        // kernel delta 400 (of which 100 idle) + user delta 100 => total 500.
        let prev = (0, 0, 0);
        let now = (100, 400, 100);
        assert_eq!(cpu_pct_from_deltas(prev, now), 80.0);
    }

    #[test]
    fn deltas_are_taken_not_absolute_values() {
        // Same percentages, but starting from a large boot-time baseline.
        let prev = (900_000, 5_000_000, 1_000_000);
        let now = (900_250, 5_000_500, 1_000_000);
        assert_eq!(cpu_pct_from_deltas(prev, now), 50.0);
    }

    #[test]
    fn no_elapsed_ticks_is_undefined_and_reports_zero() {
        let prev = (500, 900, 100);
        assert_eq!(cpu_pct_from_deltas(prev, prev), 0.0);
    }

    #[test]
    fn backwards_counters_do_not_underflow() {
        let prev = (9_000, 9_000, 9_000);
        let now = (1, 1, 1);
        assert_eq!(cpu_pct_from_deltas(prev, now), 0.0);
    }
}
