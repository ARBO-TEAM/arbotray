//! When to put a balloon in front of the user, and what it should say.
//!
//! This is the half of the tray notification that has nothing to do with
//! Win32. `icon::Icon::balloon` knows how to show one; this decides whether
//! there is one to show, which is the part that can be got wrong in ways a
//! screenshot will not reveal.
//!
//! The whole design problem here is *restraint*. A monitor that samples once a
//! second and fires a balloon whenever a reading is high would put sixty
//! notifications on screen in the first minute, and the second one is already
//! worse than none: a user who has learned to dismiss our balloons will dismiss
//! the one that mattered. So every rule below is a rule about not firing again:
//!
//! * The data plan announces each **threshold** once, and re-arms only when the
//!   figure drops back under it — which in practice is the turn of the month.
//!   Crossing 90% does not re-announce at 91%, 92%, 93%.
//! * The rate alert fires when the line goes busy, then stays quiet for
//!   [`RATE_COOLDOWN`] no matter how long it stays busy. A download that runs
//!   for an hour is one event, not thirty-six hundred.
//!
//! Nothing in this module talks to the shell, so all of it is unit-tested with
//! synthetic clocks rather than by watching a corner of a screen.

use crate::config::Notify;
use std::time::{Duration, Instant};

/// How long the rate alert stays quiet after firing, however busy the line is.
///
/// Five minutes rather than a minute: this alert exists to tell someone that
/// something started using the connection, and the answer to "it is still using
/// it" is one they already have. It is long enough that a single large download
/// produces one balloon at the start and — if it is a really long one — a
/// reminder, and short enough that two genuinely separate transfers an evening
/// apart are two events.
pub const RATE_COOLDOWN: Duration = Duration::from_secs(300);

/// The plan is *spent*, as opposed to merely close to it. Announced in its own
/// right and always, even when the user's own threshold is higher than it — a
/// threshold of 100 collapses the two into one balloon rather than firing twice.
pub const QUOTA_FULL_PCT: u32 = 100;

/// How loud a balloon is. The shell draws an icon from this, and that icon is
/// the only part of a notification most people read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Something happened. The line went busy.
    Info,
    /// Something is wrong, or about to be. The plan is running out.
    Warning,
}

/// One notification, ready to hand to the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Balloon {
    pub title: String,
    pub body: String,
    pub level: Level,
}

/// The state that keeps a balloon from repeating itself.
///
/// Held by the tray window across ticks, because every rule in this file is
/// about what *already* happened — a fresh one each sample would announce the
/// same 90% sixty times a minute, which is the exact failure this exists to
/// prevent.
#[derive(Debug, Default)]
pub struct Alerts {
    /// The highest plan threshold already announced, or `None` when the plan is
    /// under the user's threshold and everything is re-armed.
    quota_fired: Option<u32>,
    /// Whether the last sample was over the rate threshold. A falling edge is
    /// what re-arms the alert before the cooldown is up, so a short burst,
    /// a pause, and a second burst are two events.
    rate_high: bool,
    /// When the rate alert last fired. `None` until it has.
    rate_at: Option<Instant>,
}

impl Alerts {
    pub fn new() -> Self {
        Self::default()
    }

    /// The balloon this sample earned, if any.
    ///
    /// `quota_pct` is `None` when no plan is configured, which is different
    /// from 0% — a machine with no plan can never cross a threshold, and must
    /// not be told it did.
    ///
    /// `now` is a parameter rather than read here so the cooldown can be tested
    /// without sleeping through it.
    ///
    /// At most one balloon per sample, and the plan wins: two notifications
    /// stacked in the same second is the thing that makes people switch them
    /// off, and of the two the plan is the one with a consequence.
    pub fn poll(
        &mut self,
        cfg: &Notify,
        quota_pct: Option<f32>,
        rx_bps: u64,
        now: Instant,
    ) -> Option<Balloon> {
        if !cfg.enabled {
            // Deliberately *not* a reset. Switching alerts off and on again
            // during the same month is not a reason to re-announce a threshold
            // that was already crossed and already read.
            return None;
        }
        self.quota(cfg, quota_pct).or_else(|| self.rate(cfg, rx_bps, now))
    }

    /// The data-plan half: each threshold announced once per crossing.
    fn quota(&mut self, cfg: &Notify, pct: Option<f32>) -> Option<Balloon> {
        let threshold = clamp_quota_pct(cfg.quota_pct)?;
        // No plan configured, so there is no percentage and nothing to cross.
        let pct = pct?;

        // Back under the user's own threshold: the month turned over, or the
        // plan was raised. Re-arm everything.
        if pct < threshold as f32 {
            self.quota_fired = None;
            return None;
        }

        // The highest level this sample has reached. `QUOTA_FULL_PCT` is its
        // own event, so a plan that is merely close and one that is spent do
        // not read the same.
        let level = if pct >= QUOTA_FULL_PCT as f32 {
            QUOTA_FULL_PCT.max(threshold)
        } else {
            threshold
        };
        // Already said, and saying it again on the next sample is the failure.
        if self.quota_fired.is_some_and(|fired| fired >= level) {
            return None;
        }
        self.quota_fired = Some(level);

        Some(if level >= QUOTA_FULL_PCT {
            Balloon {
                title: "Data plan spent".into(),
                body: format!(
                    "This month's traffic has reached {pct:.0}% of your plan."
                ),
                level: Level::Warning,
            }
        } else {
            Balloon {
                title: "Data plan running low".into(),
                body: format!(
                    "This month's traffic has passed {threshold}% of your plan ({pct:.0}%)."
                ),
                level: Level::Warning,
            }
        })
    }

    /// The rate half: one balloon per busy period, then quiet for the cooldown.
    fn rate(&mut self, cfg: &Notify, rx_bps: u64, now: Instant) -> Option<Balloon> {
        let Some(threshold) = rate_threshold_bps(cfg.rate_mbps) else {
            // Switched off. The edge is still cleared, so switching it on again
            // during a download announces that download rather than waiting for
            // the next one.
            self.rate_high = false;
            return None;
        };

        if rx_bps < threshold {
            self.rate_high = false;
            return None;
        }

        // Rising edge, or the cooldown has run out under a line that never went
        // quiet. Either is an event worth one balloon.
        let due = match self.rate_at {
            Some(last) => now.duration_since(last) >= RATE_COOLDOWN,
            None => true,
        };
        if self.rate_high && !due {
            return None;
        }
        self.rate_high = true;
        self.rate_at = Some(now);

        Some(Balloon {
            title: "Download running".into(),
            body: format!(
                "Incoming traffic is at {}, over your {} alert.",
                crate::taskbar::format_rate(rx_bps),
                crate::taskbar::format_rate(threshold)
            ),
            level: Level::Info,
        })
    }
}

/// The plan threshold as a usable percentage, or `None` when it is switched off.
///
/// `0` is the off value rather than "alert immediately": a threshold of zero is
/// crossed by the first byte of the month, which is not a warning anybody asked
/// for. Anything above `QUOTA_FULL_PCT` is pulled back to it, because a plan can
/// be exceeded but a *warning* past the point of being spent has nothing left to
/// warn about.
pub fn clamp_quota_pct(pct: u32) -> Option<u32> {
    (pct > 0).then(|| pct.min(QUOTA_FULL_PCT))
}

/// The rate threshold in bytes per second, or `None` when it is switched off.
///
/// MB/s in, bytes out, and the MB is a mebibyte — the same unit `format_rate`
/// prints, so a user who types the number they saw on the taskbar gets the
/// alert they expected. A number that is not a positive, finite quantity is the
/// off value: this is a hand-editable file, and `NaN` has to land as "no alert"
/// rather than as a comparison that is false forever in a way nobody can see.
pub fn rate_threshold_bps(mbps: f64) -> Option<u64> {
    const MIB: f64 = 1024.0 * 1024.0;
    if !mbps.is_finite() || mbps <= 0.0 {
        return None;
    }
    Some((mbps * MIB) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() -> Notify {
        Notify {
            enabled: true,
            quota_pct: 90,
            rate_mbps: 5.0,
        }
    }

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn a_plan_under_its_threshold_says_nothing() {
        let mut a = Alerts::new();
        assert_eq!(a.poll(&on(), Some(89.9), 0, t0()), None);
    }

    #[test]
    fn crossing_the_threshold_fires_once_and_not_again() {
        let mut a = Alerts::new();
        let now = t0();
        let first = a.poll(&on(), Some(90.0), 0, now);
        assert!(first.is_some(), "the crossing itself has to be announced");
        assert_eq!(first.unwrap().level, Level::Warning);
        // Every sample after it is the same crossing, not a new one.
        assert_eq!(a.poll(&on(), Some(91.0), 0, now), None);
        assert_eq!(a.poll(&on(), Some(95.0), 0, now), None);
        assert_eq!(a.poll(&on(), Some(99.9), 0, now), None);
    }

    #[test]
    fn a_spent_plan_is_its_own_event_after_a_low_warning() {
        // 90% and 100% are different things to be told, so the second one gets
        // through even though the first already fired.
        let mut a = Alerts::new();
        let now = t0();
        assert!(a.poll(&on(), Some(92.0), 0, now).is_some());
        let full = a.poll(&on(), Some(100.0), 0, now).expect("100% is its own event");
        assert_eq!(full.title, "Data plan spent");
        // And that one does not repeat either, however far over it goes.
        assert_eq!(a.poll(&on(), Some(140.0), 0, now), None);
    }

    #[test]
    fn a_threshold_of_a_hundred_does_not_announce_the_same_thing_twice() {
        // The user's threshold and the spent mark are the same number here, so
        // there is one event, not two.
        let cfg = Notify { quota_pct: 100, ..on() };
        let mut a = Alerts::new();
        let now = t0();
        let fired = a.poll(&cfg, Some(100.0), 0, now).expect("spent is still announced");
        assert_eq!(fired.title, "Data plan spent");
        assert_eq!(a.poll(&cfg, Some(101.0), 0, now), None);
    }

    #[test]
    fn a_new_month_re_arms_the_plan_alert() {
        let mut a = Alerts::new();
        let now = t0();
        assert!(a.poll(&on(), Some(100.0), 0, now).is_some());
        assert_eq!(a.poll(&on(), Some(100.0), 0, now), None);
        // The counter resets and the figure drops back under the threshold.
        assert_eq!(a.poll(&on(), Some(0.4), 0, now), None);
        assert!(
            a.poll(&on(), Some(90.0), 0, now).is_some(),
            "next month's crossing is a new event"
        );
    }

    #[test]
    fn no_plan_configured_can_never_cross_a_threshold() {
        // `None` is "there is no plan", which must not read as 0% — and must
        // certainly not read as a crossing.
        let mut a = Alerts::new();
        assert_eq!(a.poll(&on(), None, 0, t0()), None);
    }

    #[test]
    fn a_zero_threshold_switches_the_plan_alert_off_rather_than_firing_at_once() {
        let cfg = Notify { quota_pct: 0, ..on() };
        let mut a = Alerts::new();
        assert_eq!(a.poll(&cfg, Some(100.0), 0, t0()), None);
        assert_eq!(clamp_quota_pct(0), None);
    }

    #[test]
    fn a_threshold_over_a_hundred_is_pulled_back_to_it() {
        assert_eq!(clamp_quota_pct(150), Some(100));
        assert_eq!(clamp_quota_pct(100), Some(100));
        assert_eq!(clamp_quota_pct(1), Some(1));
    }

    #[test]
    fn the_rate_alert_fires_on_the_rising_edge_only() {
        let mut a = Alerts::new();
        let now = t0();
        let cfg = Notify { quota_pct: 0, ..on() };
        let busy = 6 * 1024 * 1024;
        let fired = a.poll(&cfg, None, busy, now).expect("going busy is an event");
        assert_eq!(fired.level, Level::Info);
        // Still busy a second later is the same event.
        assert_eq!(a.poll(&cfg, None, busy, now + Duration::from_secs(1)), None);
        assert_eq!(a.poll(&cfg, None, busy, now + Duration::from_secs(60)), None);
    }

    #[test]
    fn a_line_that_stays_busy_past_the_cooldown_is_reminded_once() {
        let mut a = Alerts::new();
        let now = t0();
        let cfg = Notify { quota_pct: 0, ..on() };
        let busy = 6 * 1024 * 1024;
        assert!(a.poll(&cfg, None, busy, now).is_some());
        assert_eq!(a.poll(&cfg, None, busy, now + RATE_COOLDOWN - Duration::from_secs(1)), None);
        assert!(
            a.poll(&cfg, None, busy, now + RATE_COOLDOWN).is_some(),
            "the cooldown is up, so one reminder gets through"
        );
        // And the clock restarts from the reminder, not from the first balloon.
        assert_eq!(a.poll(&cfg, None, busy, now + RATE_COOLDOWN + Duration::from_secs(1)), None);
    }

    #[test]
    fn going_quiet_re_arms_the_rate_alert_without_waiting_for_the_cooldown() {
        // Two separate downloads a minute apart are two events. Waiting out the
        // cooldown here would silence the second one entirely.
        let mut a = Alerts::new();
        let now = t0();
        let cfg = Notify { quota_pct: 0, ..on() };
        let busy = 6 * 1024 * 1024;
        assert!(a.poll(&cfg, None, busy, now).is_some());
        assert_eq!(a.poll(&cfg, None, 1024, now + Duration::from_secs(30)), None);
        assert!(
            a.poll(&cfg, None, busy, now + Duration::from_secs(60)).is_some(),
            "a new burst after a quiet spell is a new event"
        );
    }

    #[test]
    fn a_rate_under_the_threshold_says_nothing() {
        let mut a = Alerts::new();
        let cfg = Notify { quota_pct: 0, ..on() };
        // 5 MB/s configured, so anything under 5 MiB/s is quiet.
        assert_eq!(a.poll(&cfg, None, 5 * 1024 * 1024 - 1, t0()), None);
    }

    #[test]
    fn the_rate_threshold_is_read_in_the_same_unit_the_taskbar_prints() {
        // A user types the number they saw on the strip, so `5` here has to
        // mean the same thing as `5.0M/s` there.
        assert_eq!(rate_threshold_bps(5.0), Some(5 * 1024 * 1024));
        assert_eq!(rate_threshold_bps(0.5), Some(512 * 1024));
    }

    #[test]
    fn a_nonsense_rate_threshold_switches_the_alert_off_rather_than_never_matching() {
        // The file is hand-editable, and a NaN compares false against every
        // rate forever — an alert that is silently dead rather than switched off.
        assert_eq!(rate_threshold_bps(f64::NAN), None);
        assert_eq!(rate_threshold_bps(-1.0), None);
        assert_eq!(rate_threshold_bps(0.0), None);
        assert_eq!(rate_threshold_bps(f64::INFINITY), None);
    }

    #[test]
    fn switching_alerts_off_silences_both_halves() {
        let cfg = Notify { enabled: false, ..on() };
        let mut a = Alerts::new();
        assert_eq!(a.poll(&cfg, Some(100.0), 99 * 1024 * 1024, t0()), None);
    }

    #[test]
    fn switching_alerts_back_on_does_not_re_announce_what_was_already_read() {
        let mut a = Alerts::new();
        let now = t0();
        assert!(a.poll(&on(), Some(95.0), 0, now).is_some());
        let off = Notify { enabled: false, ..on() };
        assert_eq!(a.poll(&off, Some(95.0), 0, now), None);
        assert_eq!(
            a.poll(&on(), Some(95.0), 0, now),
            None,
            "the same crossing is not a new event because a checkbox moved"
        );
    }

    #[test]
    fn the_plan_takes_precedence_when_both_would_fire() {
        // One balloon per sample. Two stacked in the same second is what makes
        // people switch notifications off, and the plan is the one with a
        // consequence attached to it.
        let mut a = Alerts::new();
        let fired = a
            .poll(&on(), Some(100.0), 99 * 1024 * 1024, t0())
            .expect("something fires");
        assert_eq!(fired.level, Level::Warning);
        assert!(fired.title.starts_with("Data plan"));
    }

    #[test]
    fn a_suppressed_rate_alert_is_not_consumed_by_the_plan_firing_first() {
        // The plan won this sample, so the rate alert has *not* fired and its
        // edge must still be un-armed — otherwise the download that was running
        // at the moment the plan filled up would never be announced.
        let mut a = Alerts::new();
        let now = t0();
        let busy = 99 * 1024 * 1024;
        assert!(a.poll(&on(), Some(100.0), busy, now).is_some());
        let second = a.poll(&on(), Some(100.0), busy, now).expect("the rate's turn");
        assert_eq!(second.level, Level::Info);
    }

    #[test]
    fn a_balloon_says_what_it_is_about_without_the_number_alone() {
        // `szInfo` is plain text with no formatting, so the sentence is the
        // whole message: a body reading "90%" would be a notification the user
        // has to guess the subject of.
        let mut a = Alerts::new();
        let fired = a.poll(&on(), Some(93.0), 0, t0()).unwrap();
        assert!(fired.body.contains("plan"), "{}", fired.body);
        assert!(fired.body.contains("93%"), "{}", fired.body);
        assert!(!fired.title.is_empty());
    }
}
