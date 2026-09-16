//! The Stopwatch page's clock.
//!
//! It lives in `UiState` rather than inside a control, for the same reason the
//! model does: the dashboard is driven by direct calls on the tray's own thread,
//! so a value nobody else reads has no reason to be a window of its own.
//!
//! Only the accumulated time and the instant it was last started are kept. The
//! displayed value is derived from those two on every read rather than advanced
//! by a tick, so the clock cannot drift against the wall: a tick that arrives
//! late shows a larger elapsed time, not a smaller one, and a tick that never
//! arrives at all costs nothing but a stale label.

use std::time::{Duration, Instant};

/// A running-or-paused count of elapsed time.
#[derive(Default)]
pub(crate) struct Stopwatch {
    /// Time accumulated by the runs that have already been stopped.
    elapsed: Duration,
    /// When the current run started, or `None` when it is not running.
    started: Option<Instant>,
}

impl Stopwatch {
    /// The time on the clock right now, including a run in progress.
    pub(crate) fn elapsed(&self) -> Duration {
        match self.started {
            Some(at) => self.elapsed + at.elapsed(),
            None => self.elapsed,
        }
    }

    pub(crate) fn is_running(&self) -> bool {
        self.started.is_some()
    }

    /// Start it if it is stopped, stop it if it is running. Returns the new
    /// running flag, so the button's own label follows the same answer the
    /// clock did rather than keeping a second copy of it.
    pub(crate) fn toggle(&mut self) -> bool {
        match self.started.take() {
            // Stopping banks the run before clearing it, so the time already
            // counted is not lost with the start instant.
            Some(at) => self.elapsed += at.elapsed(),
            None => self.started = Some(Instant::now()),
        }
        self.is_running()
    }

    /// Back to zero, and stopped.
    ///
    /// Stopped rather than left running: a reset that started counting again on
    /// its own would be a second Start, and the button would be showing the
    /// wrong word for it.
    pub(crate) fn reset(&mut self) {
        self.elapsed = Duration::ZERO;
        self.started = None;
    }

    /// The clock as it is drawn. `M:SS.d` under an hour, `H:MM:SS` past it.
    ///
    /// The tenths stop at the hour because the hour is where the line has to
    /// carry a field the reader is actually using — and a tenth that only moves
    /// ten times a second is not what a run of an hour is being watched for.
    pub(crate) fn text(&self, running: bool) -> String {
        let total = self.elapsed();
        let secs = total.as_secs();
        let tenths = total.subsec_millis() / 100;
        if secs < 3600 {
            // A stopped clock is shown to the tenth it stopped on, and a
            // tenth is a digit that never changes again — so it is rounded off
            // instead, and the readout does not claim a precision the pause
            // makes meaningless.
            if running {
                format!("{}:{:02}.{}", secs / 60, secs % 60, tenths)
            } else {
                format!("{}:{:02}", secs / 60, secs % 60)
            }
        } else {
            // Both states print the hour field: past the hour the tenths are
            // gone either way, so run and pause read the same.
            format!("{}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60)
        }
    }

    /// What the Start/Stop button says for a given running state.
    ///
    /// An associated function rather than a method on a *state*: it maps a
    /// running flag to a word, and the clamp on a `Deref`-less value object is
    /// that the caller already has the flag, so no clock is needed to read it.
    pub(crate) fn label(running: bool) -> &'static str {
        if running { "Stop" } else { "Start" }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clock with no run behind it, and its two neighbours.
    fn at(elapsed: Duration) -> Stopwatch {
        Stopwatch { elapsed, started: None }
    }

    #[test]
    fn a_clock_that_has_never_run_starts_at_zero() {
        let watch = Stopwatch::default();
        assert_eq!(watch.elapsed(), Duration::ZERO);
        assert!(!watch.is_running());
        assert_eq!(watch.text(false), "0:00");
    }

    #[test]
    fn toggling_reports_the_state_the_clock_is_now_in() {
        // The button's label comes from this return value. A clock that
        // flipped its own state and answered with the old one would read
        // "Start" while it was counting.
        let mut watch = Stopwatch::default();
        assert!(watch.toggle(), "a stopped clock starts");
        assert!(watch.is_running());
        assert!(!watch.toggle(), "a running clock stops");
        assert!(!watch.is_running());
    }

    #[test]
    fn stopping_keeps_the_time_already_counted() {
        // The start instant is dropped on stop, so the run has to be banked
        // before it goes. Without that the clock would snap back to whatever
        // it read when the run began.
        let mut watch = Stopwatch { elapsed: Duration::from_secs(5), started: None };
        watch.toggle();
        std::thread::sleep(Duration::from_millis(20));
        watch.toggle();
        assert!(
            watch.elapsed() >= Duration::from_millis(5020),
            "the run was dropped on stop: {:?}",
            watch.elapsed()
        );
        // And restarting continues from there rather than from zero.
        watch.toggle();
        assert!(watch.elapsed() >= Duration::from_millis(5020));
    }

    #[test]
    fn a_reset_stops_the_clock_as_well_as_zeroing_it() {
        // Left running, the next read would already be non-zero and the page
        // would look like it had restarted itself.
        let mut watch = Stopwatch { elapsed: Duration::from_secs(30), started: None };
        watch.toggle();
        watch.reset();
        assert_eq!(watch.elapsed(), Duration::ZERO);
        assert!(!watch.is_running());
        assert_eq!(watch.text(false), "0:00");
    }

    #[test]
    fn the_running_reading_carries_tenths_under_the_hour() {
        let mut watch = at(Duration::from_millis(65_400));
        watch.started = Some(Instant::now());
        assert_eq!(watch.text(true), "1:05.4");
        assert_eq!(watch.text(false), "1:05");
    }

    #[test]
    fn past_the_hour_the_reading_switches_to_hours() {
        // The boundary is where `M:SS.d` would still print — 60:00.0 is a
        // minute field of 60, which is the one reading that looks like a bug.
        let watch = at(Duration::from_secs(3_599));
        assert_eq!(watch.text(false), "59:59");
        let watch = at(Duration::from_secs(3_600));
        assert_eq!(watch.text(false), "1:00:00");
        let watch = at(Duration::from_secs(7_384));
        assert_eq!(watch.text(false), "2:03:04");
    }

    #[test]
    fn the_button_says_the_opposite_of_what_the_clock_is_doing() {
        assert_eq!(Stopwatch::label(false), "Start");
        assert_eq!(Stopwatch::label(true), "Stop");
    }
}
