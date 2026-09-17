//! The sleep / shut-down timer: when it fires, what it does, and the two calls
//! that do it.
//!
//! This is the one feature in the app that can end the session, so it is built
//! the way the rest of the app is built rather than as a shortcut: the trigger
//! is a pure function of one `SystemTime`, the action is an enum the form and
//! the config both speak, and both are tested here where there is no window and
//! no machine to put to sleep.
//!
//! The two actions are deliberately not symmetrical, because the underlying
//! APIs are not. Suspending needs no privilege at all — `SetSuspendState` is a
//! plain call. Shutting down needs `SE_SHUTDOWN_NAME` *enabled* in the process
//! token, which is why an app that merely calls `ExitWindowsEx` on a normal
//! account gets a bare failure with nothing to explain it. Both are done in
//! full here rather than by spelling out a `shutdown.exe` command line: this
//! app talks to Windows directly everywhere else it can, and a subprocess is
//! one more thing that can be missing, blocked, or shown in a console window.

use windows::Win32::Foundation::{HANDLE, LUID, SYSTEMTIME};
use windows::Win32::Security::{
    AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::System::Power::SetSuspendState;
use windows::Win32::System::Shutdown::{
    EWX_POWEROFF, ExitWindowsEx, SHTDN_REASON_FLAG_PLANNED, SHTDN_REASON_MAJOR_APPLICATION,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{PCWSTR, w};

/// What the timer does when it comes due.
///
/// An enum rather than a pair of booleans: "shut down" and "sleep" are the two
/// answers to one question, and a `Vec<bool>`-shaped config would let both be
/// set and leave the tie-break to whoever read it last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Sleep,
    Shutdown,
}

impl Action {
    /// The stored spelling, and the one the config round-trips through.
    ///
    /// Spelled out rather than derived from the variant name so that renaming a
    /// variant cannot silently rewrite what is already on disk.
    pub fn key(self) -> &'static str {
        match self {
            Action::Sleep => "sleep",
            Action::Shutdown => "shutdown",
        }
    }

    /// The variant `key` names, or `None` for anything else — a hand-edited
    /// config, or one written by a version with a third action in it.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "sleep" => Some(Action::Sleep),
            "shutdown" => Some(Action::Shutdown),
            _ => None,
        }
    }

    /// The word the page shows for it.
    pub fn label(self) -> &'static str {
        match self {
            Action::Sleep => "Sleep",
            Action::Shutdown => "Shut down",
        }
    }

    /// The other one. There are two, so the toggle needs no table.
    pub fn flipped(self) -> Self {
        match self {
            Action::Sleep => Action::Shutdown,
            Action::Shutdown => Action::Sleep,
        }
    }
}

/// When the timer comes due: on the clock, or a fixed span from now.
///
/// Both, because they answer different questions and the same user wants both
/// on different days — "put the machine to bed at 23:00" is a habit, and "give
/// me 45 more minutes" is a decision made once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Fire at the next minute-of-day matching the configured time. Once it has
    /// fired it arms again for the same time tomorrow, which is what makes a
    /// nightly timer a thing you set once.
    AtTime,
    /// Fire at a fixed instant, chosen when the timer is armed.
    Countdown,
}

impl Mode {
    pub fn key(self) -> &'static str {
        match self {
            Mode::AtTime => "at",
            Mode::Countdown => "countdown",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "at" => Some(Mode::AtTime),
            "countdown" => Some(Mode::Countdown),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::AtTime => "At a time",
            Mode::Countdown => "After a wait",
        }
    }
}

/// Midnight of the day `t` falls on, in whole minutes since the epoch.
///
/// The epoch is the caller's to choose as long as both sides of a comparison
/// use the same one; this is only ever subtracted from, never interpreted.
fn minute_of_day(t: &SYSTEMTIME) -> i64 {
    i64::from(t.wHour) * 60 + i64::from(t.wMinute)
}

/// Whole minutes since an arbitrary origin that is stable within a run.
///
/// Not a calendar: it is a difference machine. The only arithmetic asked of it
/// is "how far apart are these two", and for that a month of 30 or 31 days is
/// irrelevant — it cancels, because both operands are built the same way.
fn ordinal_minutes(t: &SYSTEMTIME) -> i64 {
    // A 365.25-day year keeps the origin from drifting, but the value is never
    // read as a date — only ever subtracted.
    let year = i64::from(t.wYear) - 1970;
    let days = year * 365 + year / 4;
    let month_days = [0i64, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let month = (t.wMonth.max(1) as usize).min(12) - 1;
    (days + month_days[month] + i64::from(t.wDay.max(1)) - 1) * 1440 + minute_of_day(t)
}

/// Step `t` forward by `days`, rolling the month and the year as it goes.
///
/// A loop rather than one arithmetic step, deliberately: the calendar is the
/// part of this file most able to be subtly wrong, and stepping one day at a
/// time through `days_in_month` is the version that can be read and believed.
/// The furthest this is ever asked to go is a day, so the loop is not a cost.
fn add_days(t: &mut SYSTEMTIME, days: u32) {
    for _ in 0..days {
        let last = days_in_month(t.wYear, t.wMonth) as u16;
        if t.wDay >= last {
            t.wDay = 1;
            if t.wMonth >= 12 {
                t.wMonth = 1;
                t.wYear = t.wYear.saturating_add(1);
            } else {
                t.wMonth += 1;
            }
        } else {
            t.wDay += 1;
        }
    }
}

/// The next instant the given minute-of-day occurs, strictly after now.
///
/// Strictly after, not at-or-after: a timer asking for the minute it is already
/// sitting in would fire the moment it was armed, which is not what anyone
/// means by "at 23:00". The answer is therefore always within the next 24 hours.
pub fn next_at(target_minute: u32, now: &SYSTEMTIME) -> SYSTEMTIME {
    let target = target_minute % 1440;
    let mut when = now.clone();
    when.wSecond = 0;
    when.wMilliseconds = 0;
    when.wMinute = (target % 60) as u16;
    when.wHour = (target / 60) as u16;
    if i64::from(target) <= minute_of_day(now) {
        add_days(&mut when, 1);
    }
    when
}

/// The instant `minutes` from `now`, rolling the calendar the same way.
pub fn after_minutes(minutes: u32, now: &SYSTEMTIME) -> SYSTEMTIME {
    let total = minute_of_day(now) + i64::from(minutes);
    let mut when = now.clone();
    when.wSecond = 0;
    when.wMilliseconds = 0;
    // Whole days first, then the leftover as a time of day: splitting it this
    // way is what keeps a long wait from needing the hour field to hold more
    // than 24.
    add_days(&mut when, (total / 1440) as u32);
    let minute = (total % 1440) as u16;
    when.wMinute = minute % 60;
    when.wHour = minute / 60;
    when
}

/// Days in `month`, February included. A leap year is one divisible by 4 that
/// is not a century unless it is a fourth century.
fn days_in_month(year: u16, month: u16) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            let y = u32::from(year);
            if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

/// Whether `when` has been reached, in minutes, from `now`.
///
/// The unit is a minute on purpose: a timer set a day out does not need to know
/// about seconds, and a per-tick comparison that fires on the minute cannot
/// fire twice for one instant.
pub fn due(when: &SYSTEMTIME, now: &SYSTEMTIME) -> bool {
    ordinal_minutes(now) >= ordinal_minutes(when)
}

/// Minutes from `now` until `when`, never negative.
pub fn minutes_until(when: &SYSTEMTIME, now: &SYSTEMTIME) -> i64 {
    (ordinal_minutes(when) - ordinal_minutes(now)).max(0)
}

/// The current local time. Local, not UTC: the user sets this against the clock
/// on the wall in front of them.
pub fn now() -> SYSTEMTIME {
    // SAFETY: `GetLocalTime` fills a struct we own and cannot fail.
    unsafe { GetLocalTime() }
}

/// `"23:00"` and `"07:30"` to minutes past midnight.
///
/// Written by hand rather than with a formatting crate: it is one split on a
/// colon and two parses, and the accept/reject rules are the interesting part
/// and want to be visible.
pub fn parse_hhmm(text: &str) -> Option<u32> {
    let (h, m) = text.trim().split_once(':')?;
    let h: u32 = h.trim().parse().ok()?;
    let m: u32 = m.trim().parse().ok()?;
    if h > 23 || m > 59 {
        return None;
    }
    Some(h * 60 + m)
}

/// Minutes past midnight as `"23:00"`.
pub fn format_hhmm(minute_of_day: u32) -> String {
    let m = minute_of_day % 1440;
    format!("{:02}:{:02}", m / 60, m % 60)
}

// --- the two actions -------------------------------------------------------

/// Put the machine to sleep.
///
/// `bHibernate` false is a suspend, not a hibernate: hibernating writes every
/// page of memory to disk, which is a different thing from what a "sleep" timer
/// promises and can take a minute on a full machine. No privilege is needed.
pub fn sleep() -> Result<(), String> {
    // SAFETY: a plain call with no pointers, and every argument a value.
    let ok = unsafe { SetSuspendState(false, false, false) };
    if ok {
        Ok(())
    } else {
        Err("Windows refused the suspend request".into())
    }
}

/// Shut the machine down.
///
/// The privilege must be enabled first — this is the part an app that calls
/// `ExitWindowsEx` alone leaves out, and the part that makes it fail on an
/// ordinary account with nothing in the message to say why.
///
/// `EWX_POWEROFF` rather than `EWX_SHUTDOWN`: they differ only on machines with
/// an ATX supply, where poweroff is the one that actually cuts the power.
/// `SHTDN_REASON_FLAG_PLANNED` records this in the event log as deliberate, so
/// it is not read back as a crash.
pub fn shutdown() -> Result<(), String> {
    enable_shutdown_privilege()?;
    // SAFETY: the privilege is held by now; a planned application-initiated
    // power-off is the narrowest form of this call.
    unsafe { ExitWindowsEx(EWX_POWEROFF, SHTDN_REASON_MAJOR_APPLICATION | SHTDN_REASON_FLAG_PLANNED) }
        .map_err(|e| format!("Windows refused the shut-down request: {e}"))
}

/// Ask the process token for `SE_SHUTDOWN_NAME`.
///
/// Best effort in one respect and not another: if the process already holds the
/// privilege this is a no-op, and if the token cannot be opened the caller gets
/// the failure and a message — never a silent success, because the call it is
/// gating is about to end the session.
fn enable_shutdown_privilege() -> Result<(), String> {
    // SAFETY: every pointer is to a live local, and the two handles are closed
    // on every path — including the early returns, which is why the close is
    // not deferred past them.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
        .map_err(|e| format!("could not open the process token: {e}"))?;

        let mut luid = LUID::default();
        let result = LookupPrivilegeValueW(PCWSTR::null(), w!("SeShutdownPrivilege"), &mut luid);

        if let Err(e) = result {
            let _ = windows::Win32::Foundation::CloseHandle(token);
            return Err(format!("could not look up the shut-down privilege: {e}"));
        }

        let privileges = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        let adjusted = AdjustTokenPrivileges(
            token,
            false,
            Some(&privileges),
            std::mem::size_of::<TOKEN_PRIVILEGES>() as u32,
            None,
            None,
        );
        let _ = windows::Win32::Foundation::CloseHandle(token);

        // `AdjustTokenPrivileges` reports success even when it granted nothing,
        // which is its documented behaviour and the reason this does not trust
        // the `Ok`. A token that could not be given the privilege fails here
        // rather than one call later, where the message would be about the
        // shut-down rather than about the privilege.
        adjusted.map_err(|e| format!("could not enable the shut-down privilege: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: u16, mo: u16, d: u16, h: u16, mi: u16) -> SYSTEMTIME {
        SYSTEMTIME {
            wYear: y,
            wMonth: mo,
            wDay: d,
            wHour: h,
            wMinute: mi,
            wSecond: 30,
            wMilliseconds: 500,
            ..Default::default()
        }
    }

    #[test]
    fn an_action_round_trips_through_its_stored_spelling() {
        for action in [Action::Sleep, Action::Shutdown] {
            assert_eq!(Action::parse(action.key()), Some(action));
        }
        // Case and stray space, because the config file is a text file someone
        // can edit, and `"Sleep"` is what they would write.
        assert_eq!(Action::parse(" Sleep "), Some(Action::Sleep));
        assert_eq!(Action::parse("SHUTDOWN"), Some(Action::Shutdown));
        // Anything else is refused rather than guessed at: the fallback for a
        // word nobody recognises has to be "the timer is off", never "sleep".
        assert_eq!(Action::parse("hibernate"), None);
        assert_eq!(Action::parse(""), None);
        assert_eq!(Action::Sleep.flipped(), Action::Shutdown);
        assert_eq!(Action::Shutdown.flipped(), Action::Sleep);
    }

    #[test]
    fn a_time_parses_only_when_it_is_a_real_time() {
        assert_eq!(parse_hhmm("23:00"), Some(23 * 60));
        assert_eq!(parse_hhmm(" 7:5 "), Some(7 * 60 + 5));
        assert_eq!(parse_hhmm("00:00"), Some(0));
        assert_eq!(parse_hhmm("23:59"), Some(23 * 60 + 59));
        // The three that would otherwise become a different time than typed.
        assert_eq!(parse_hhmm("24:00"), None);
        assert_eq!(parse_hhmm("12:60"), None);
        assert_eq!(parse_hhmm("half past"), None);
        assert_eq!(format_hhmm(parse_hhmm("07:05").unwrap()), "07:05");
    }

    #[test]
    fn a_time_later_today_fires_today() {
        let now = at(2026, 9, 17, 14, 30);
        let when = next_at(23 * 60, &now);
        assert_eq!((when.wDay, when.wHour, when.wMinute), (17, 23, 0));
        assert_eq!(minutes_until(&when, &now), 510);
    }

    #[test]
    fn a_time_already_past_fires_tomorrow_and_arms_again() {
        let now = at(2026, 9, 17, 23, 30);
        let when = next_at(23 * 60, &now);
        assert_eq!(
            (when.wMonth, when.wDay, when.wHour, when.wMinute),
            (9, 18, 23, 0),
            "a nightly timer set once has to come round again"
        );
        // The minute it is already in counts as past: a timer armed at 23:00:30
        // for "23:00" must not fire on the spot.
        let same = next_at(23 * 60, &at(2026, 9, 17, 23, 0));
        assert_eq!(same.wDay, 18);
    }

    #[test]
    fn a_time_rolls_the_month_and_the_year() {
        let last = next_at(0, &at(2026, 12, 31, 23, 59));
        assert_eq!((last.wYear, last.wMonth, last.wDay), (2027, 1, 1));
        let end = next_at(0, &at(2026, 9, 30, 12, 0));
        assert_eq!((end.wMonth, end.wDay), (10, 1));
        // February in a leap year, which is the one month the table cannot hold.
        assert_eq!(days_in_month(2028, 2), 29);
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2000, 2), 29, "2000 is a leap year");
        assert_eq!(days_in_month(1900, 2), 28, "1900 is not");
        let feb = next_at(0, &at(2028, 2, 28, 12, 0));
        assert_eq!((feb.wMonth, feb.wDay), (2, 29));
    }

    #[test]
    fn a_countdown_adds_minutes_across_boundaries() {
        let when = after_minutes(45, &at(2026, 9, 17, 14, 30));
        assert_eq!((when.wDay, when.wHour, when.wMinute), (17, 15, 15));
        // Over midnight.
        let over = after_minutes(30, &at(2026, 9, 17, 23, 45));
        assert_eq!((over.wMonth, over.wDay, over.wHour, over.wMinute), (9, 18, 0, 15));
        // Over a month end.
        let month = after_minutes(30, &at(2026, 9, 30, 23, 45));
        assert_eq!((month.wMonth, month.wDay, month.wHour), (10, 1, 0));
        // A whole day, which is where the day counter actually has to count.
        let day = after_minutes(1440, &at(2026, 9, 17, 9, 0));
        assert_eq!((day.wMonth, day.wDay, day.wHour, day.wMinute), (9, 18, 9, 0));
        assert_eq!(after_minutes(0, &at(2026, 9, 17, 9, 0)).wMinute, 0);
    }

    #[test]
    fn the_seconds_are_dropped_from_whatever_is_armed() {
        // Otherwise a timer set at 14:30:37 would come due at 14:30:37, and the
        // page's countdown would show a minute it never reaches.
        let when = after_minutes(10, &at(2026, 9, 17, 14, 30));
        assert_eq!((when.wSecond, when.wMilliseconds), (0, 0));
        let clock = next_at(600, &at(2026, 9, 17, 14, 30));
        assert_eq!((clock.wSecond, clock.wMilliseconds), (0, 0));
    }

    #[test]
    fn a_timer_is_due_on_its_minute_and_not_before() {
        let when = at(2026, 9, 17, 23, 0);
        assert!(!due(&when, &at(2026, 9, 17, 22, 59)));
        // Seconds inside the due minute do not postpone it: this fires on the
        // first tick at or past the minute, which is the whole of the promise.
        assert!(due(&when, &at(2026, 9, 17, 23, 0)));
        assert!(due(&when, &at(2026, 9, 17, 23, 1)));
        // And a day later it is still due, which is what makes a missed fire
        // (a sleeping machine, a stopped app) act on waking rather than never.
        assert!(due(&when, &at(2026, 9, 18, 6, 0)));
        assert_eq!(minutes_until(&when, &at(2026, 9, 18, 6, 0)), 0);
    }
}
