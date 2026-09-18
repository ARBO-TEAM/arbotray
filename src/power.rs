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

    /// The other one. Two variants, so the toggle needs no table.
    pub fn flipped(self) -> Self {
        match self {
            Mode::AtTime => Mode::Countdown,
            Mode::Countdown => Mode::AtTime,
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
///
/// The seconds are **kept**, unlike `next_at`'s. A countdown armed at 14:30:37
/// for forty-five minutes is due at 15:15:37, and a countdown is the one kind
/// of timer whose whole promise is the length of the wait — rounding it down to
/// the minute would make "45 minutes" a number that is only sometimes true.
pub fn after_minutes(minutes: u32, now: &SYSTEMTIME) -> SYSTEMTIME {
    let total = minute_of_day(now) + i64::from(minutes);
    let mut when = now.clone();
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

/// Seconds from `now` until `when`, negative once `when` has passed.
///
/// The whole countdown rests on this, and it is arithmetic on *calendar fields*
/// rather than on an instant. `ordinal_minutes` is a local wall-clock ordinal,
/// so a difference of minutes is exact in the frame the user set the timer in,
/// and the two `wSecond` fields carry the part of a minute still to run. No
/// timezone conversion and no `FILETIME`: a timer set for "23:00" is set against
/// the clock on the wall, and that is the only clock it ever has to agree with.
///
/// Exactness rests on `next_at` and `after_minutes` zeroing what they should:
/// a clock timer carries a zero second, so its countdown reaches 00:00 exactly
/// on its own minute.
pub fn seconds_between(when: &SYSTEMTIME, now: &SYSTEMTIME) -> i64 {
    (ordinal_minutes(when) - ordinal_minutes(now)) * 60 + i64::from(when.wSecond)
        - i64::from(now.wSecond)
}

/// Seconds from `now` until `when`, never negative — what the page counts down.
pub fn seconds_until(when: &SYSTEMTIME, now: &SYSTEMTIME) -> i64 {
    seconds_between(when, now).max(0)
}

/// How long before its target a timer raises its question.
///
/// A timer that shut the machine down the instant it came due would be a timer
/// nobody could set safely: a typo in one digit of "23:00" would end the
/// session before the mistake could be seen. This is the safety margin, and it
/// is half the reason this feature has a confirmation at all.
pub const WARN_SECS: i64 = 60;

/// How late a target may be reached and still act.
///
/// The other half of the safety margin, and the less obvious one. A laptop
/// suspended at 22:00 with a timer set for 23:00 wakes in the morning with the
/// target eleven hours in the past — and a plain "has it been reached" test
/// would put it straight back to sleep, which is the worst thing this feature
/// could do. Past this window the moment is gone and the timer is dropped
/// rather than honoured.
pub const GRACE_SECS: i64 = 300;

/// What a timer's target means right now.
///
/// One pure function rather than three predicates, because the four cases are
/// exclusive and a caller that could ask them separately would eventually ask
/// them in the wrong order — and the cost of getting *that* wrong is a machine
/// that either sleeps without asking or never sleeps at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Too far out to do anything about yet.
    Waiting,
    /// Close enough that the question should be on screen.
    Warning,
    /// Reached, and soon enough after to act on.
    Due,
    /// Passed by more than `GRACE_SECS`: the moment is gone.
    Stale,
}

/// Which of the four `when` is in, seen from `now`.
pub fn phase(when: &SYSTEMTIME, now: &SYSTEMTIME) -> Phase {
    let secs = seconds_between(when, now);
    if secs > WARN_SECS {
        Phase::Waiting
    } else if secs > 0 {
        Phase::Warning
    } else if secs >= -GRACE_SECS {
        // Includes exactly zero: the target arrives on the tick at or just
        // after its second, and the popup counts down to that.
        Phase::Due
    } else {
        Phase::Stale
    }
}

/// Whether `when` has been reached and is still worth acting on.
pub fn due(when: &SYSTEMTIME, now: &SYSTEMTIME) -> bool {
    phase(when, now) == Phase::Due
}

/// A countdown as the page prints it: `MM:SS`, `H:MM:SS`, or `Nd HH:MM:SS`.
///
/// The day field appears only past a day because that is where a reader stops
/// being able to do the arithmetic in their head, and a countdown of "172:30"
/// is a number nobody converts.
pub fn format_countdown(seconds: i64) -> String {
    let s = seconds.max(0);
    let (d, h, m, sec) = (
        s / 86_400,
        (s % 86_400) / 3_600,
        (s % 3_600) / 60,
        s % 60,
    );
    if d > 0 {
        format!("{d}d {h:02}:{m:02}:{sec:02}")
    } else if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m:02}:{sec:02}")
    }
}

/// The current local time. Local, not UTC: the user sets this against the clock
/// on the wall in front of them.
pub fn now() -> SYSTEMTIME {
    // SAFETY: `GetLocalTime` fills a struct we own and cannot fail.
    unsafe { GetLocalTime() }
}

// --- what the config stores --------------------------------------------------

/// An instant as the config stores it: `"2026-09-17 23:00:00"`.
///
/// Spelled out rather than as a unix time because this is a file people read and
/// edit, and because the two things it has to survive — a reboot, and a
/// hand-edited config — are both easier to get right when the stored value says
/// what it means.
pub fn format_stamp(t: &SYSTEMTIME) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}

/// The date for the Overview pill: `18 Sep 2026`.
///
/// From the same `GetLocalTime` fields as `format_stamp`, shortened to what a
/// pill has room for. No chrono: this is twelve month names, not a dependency.
pub fn today_label() -> String {
    let t = now();
    const MON: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let m = MON[t.wMonth.saturating_sub(1).min(11) as usize];
    format!("{:02} {} {:04}", t.wDay, m, t.wYear)
}

/// The instant a stamp names, or `None` for anything else.
///
/// A date that does not exist is refused rather than rolled forward. February
/// 30th silently becoming March 2nd is the one failure in this file that costs
/// something real, and it costs it on the day the machine goes down.
pub fn parse_stamp(text: &str) -> Option<SYSTEMTIME> {
    let (date, time) = text.trim().split_once(' ')?;
    let mut d = date.split('-');
    let (y, mo, day) = (
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
    );
    let mut t = time.split(':');
    let (h, mi, s) = (
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
        t.next().unwrap_or("0").parse().ok()?,
    );
    let parsed = SYSTEMTIME {
        wYear: y,
        wMonth: mo,
        wDay: day,
        wHour: h,
        wMinute: mi,
        wSecond: s,
        wMilliseconds: 0,
        wDayOfWeek: 0,
    };
    let sane = (1..=12).contains(&mo)
        && (1..=days_in_month(y, mo) as u16).contains(&day)
        && h <= 23
        && mi <= 59
        && s <= 59;
    sane.then_some(parsed)
}

/// The trigger a config's timer names, or the clock for anything else.
///
/// The fallback for a word nobody recognises has to be the harmless one, and
/// both of them here are chosen that way: an unknown mode is the clock, which
/// cannot fire until a time has been set, and an unknown action is sleep, which
/// does not end the session.
pub fn mode_of(timer: &crate::config::Timer) -> Mode {
    Mode::parse(&timer.mode).unwrap_or(Mode::AtTime)
}

/// The action a config's timer names, or sleep for anything else. See above.
pub fn action_of(timer: &crate::config::Timer) -> Action {
    Action::parse(&timer.action).unwrap_or(Action::Sleep)
}

/// The instant a config's timer names, or `None` when it names none.
///
/// A countdown reads its *stored* instant rather than `now` plus its wait: one
/// armed before the machine went to sleep has to fire when it wakes, on the
/// instant it was armed for. Reworking it from `now` would hand out a fresh
/// full wait every time the app restarted, and a timer that is never wrong is
/// one nobody notices is broken.
pub fn fire_at(timer: &crate::config::Timer, now: &SYSTEMTIME) -> Option<SYSTEMTIME> {
    match mode_of(timer) {
        Mode::AtTime => Some(next_at(parse_hhmm(&timer.at)?, now)),
        Mode::Countdown => parse_stamp(&timer.armed),
    }
}

/// The instant an arming performed now would target, and the one to store.
pub fn arm_target(timer: &crate::config::Timer, now: &SYSTEMTIME) -> Option<SYSTEMTIME> {
    match mode_of(timer) {
        Mode::AtTime => Some(next_at(parse_hhmm(&timer.at)?, now)),
        Mode::Countdown => Some(after_minutes(timer.after_min, now)),
    }
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

    /// A time with a ragged second and a millisecond on it, which is what a
    /// real `GetLocalTime` hands over. Every countdown test that is about *the
    /// minute* uses this, so the ragged second is present in the arithmetic
    /// rather than conveniently zero.
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

    /// The same, on the whole minute — where a countdown's arithmetic is exact.
    fn at_zero(y: u16, mo: u16, d: u16, h: u16, mi: u16) -> SYSTEMTIME {
        SYSTEMTIME {
            wSecond: 0,
            wMilliseconds: 0,
            ..at(y, mo, d, h, mi)
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
        assert_eq!(when.wSecond, 0, "a clock target is exact to the minute");
        // 510 minutes, less the 30 seconds already elapsed inside the minute
        // `now` is sitting in. The countdown is not rounded: a page that said
        // "8:30:00" at 14:30:30 would be half a minute ahead of the clock it is
        // counting against, and that is a number the user can catch lying.
        assert_eq!(seconds_until(&when, &now), 510 * 60 - 30);
        // At a whole minute, 8:30:00 exactly.
        let sharp = at_zero(2026, 9, 17, 14, 30);
        assert_eq!(seconds_until(&next_at(23 * 60, &sharp), &sharp), 510 * 60);
    }

    #[test]
    fn a_countdown_keeps_the_seconds_it_was_armed_at() {
        // The one place this differs from `next_at`, and it differs on purpose:
        // a countdown's whole promise is the *length* of the wait, so "45
        // minutes" armed at :37 is due at :37. Rounding it down to the minute
        // would make the number on the page up to 59 seconds out.
        let when = after_minutes(45, &at(2026, 9, 17, 14, 30));
        assert_eq!(when.wSecond, 30);
        assert_eq!((when.wDay, when.wHour, when.wMinute), (17, 15, 15));
        assert_eq!(seconds_until(&when, &at(2026, 9, 17, 14, 30)), 45 * 60);
    }

    #[test]
    fn a_stamp_survives_the_round_trip_it_is_stored_through() {
        let when = after_minutes(90, &at(2026, 9, 17, 22, 45));
        let text = format_stamp(&when);
        // Ninety minutes past 22:45 is a quarter past midnight *the next day*,
        // which is the half of this the calendar helper has to get right.
        assert_eq!(text, "2026-09-18 00:15:30");
        let back = parse_stamp(&text).expect("our own output must parse");
        assert_eq!(seconds_between(&back, &when), 0);
        // The instant and not just the text: this is what a restart three hours
        // later is compared against, and a stamp that read back as a different
        // day would fire on the wrong one.
        assert_eq!((back.wDay, back.wHour, back.wMinute), (18, 0, 15));
    }

    #[test]
    fn a_stamp_naming_a_day_that_does_not_exist_is_refused() {
        // The one parse in this file that costs something real when it is
        // lenient: February 30th rolled forward to March 2nd is a machine that
        // goes to sleep on a date nobody asked for.
        assert!(parse_stamp("2026-02-30 23:00:00").is_none());
        assert!(parse_stamp("2026-13-01 23:00:00").is_none());
        assert!(parse_stamp("2026-09-17 24:00:00").is_none());
        assert!(parse_stamp("2026-09-17 23:60:00").is_none());
        assert!(parse_stamp("").is_none());
        assert!(parse_stamp("tomorrow").is_none());
        assert!(parse_stamp("2026-09-17").is_none());
        // And the leap day is a real day in the year it belongs to.
        assert!(parse_stamp("2028-02-29 00:00:00").is_some());
        assert!(parse_stamp("2026-02-29 00:00:00").is_none());
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
    fn a_clock_target_drops_its_seconds_and_a_millisecond_always_goes() {
        // A clock target has to land on the minute: the page's countdown ends
        // at 00:00 and the machine has to go at that instant, not 37 seconds
        // later because that is when the timer happened to be armed.
        let clock = next_at(600, &at(2026, 9, 17, 14, 30));
        assert_eq!((clock.wSecond, clock.wMilliseconds), (0, 0));
        // A countdown keeps its second but never a millisecond: nothing on the
        // page reads finer than a second, so carrying one would only make two
        // `seconds_between` calls of the same instants disagree.
        let wait = after_minutes(10, &at(2026, 9, 17, 14, 30));
        assert_eq!(wait.wMilliseconds, 0);
        assert_eq!(wait.wSecond, 30);
    }

    #[test]
    fn a_timer_warns_a_minute_out_is_due_on_its_minute_and_gives_up_after_five() {
        let when = at_zero(2026, 9, 17, 23, 0);
        let see = |h, m, s| SYSTEMTIME {
            wSecond: s,
            wMilliseconds: 0,
            ..at_zero(2026, 9, 17, h, m)
        };

        // An hour out: nothing to say.
        assert_eq!(phase(&when, &see(22, 0, 0)), Phase::Waiting);
        // A minute and one second out is still waiting, and the second after
        // that is not — the question has to be on screen for the whole minute
        // it warns about, and that minute starts exactly 60 seconds out.
        assert_eq!(phase(&when, &see(22, 58, 59)), Phase::Waiting);
        assert_eq!(seconds_until(&when, &see(22, 59, 0)), 60);
        assert_eq!(phase(&when, &see(22, 59, 0)), Phase::Warning);
        // The target itself, and every second inside the grace window. `Due`
        // at exactly zero is what lets the popup count down *to* the instant
        // and fire on the tick that reaches it.
        assert_eq!(phase(&when, &see(23, 0, 0)), Phase::Due);
        assert!(due(&when, &see(23, 0, 0)));
        assert_eq!(phase(&when, &see(23, 4, 59)), Phase::Due);
        // And then the moment is gone.
        assert_eq!(phase(&when, &see(23, 5, 1)), Phase::Stale);
        assert!(!due(&when, &see(23, 5, 1)));
        // The case this window exists for, and the reason a plain "is it past"
        // test would be wrong: a laptop suspended before the target wakes with
        // it hours behind, and putting it straight back to sleep is the worst
        // thing this feature could do.
        assert_eq!(phase(&when, &at(2026, 9, 18, 6, 0)), Phase::Stale);
    }

    #[test]
    fn a_countdown_prints_the_fields_a_reader_actually_uses() {
        // Under a minute, minutes and seconds; under a day, hours; past a day,
        // a day field. "172:30" is the number nobody converts, which is why the
        // day unit appears at all.
        assert_eq!(format_countdown(0), "00:00");
        assert_eq!(format_countdown(-5), "00:00", "a passed timer reads zero");
        assert_eq!(format_countdown(59), "00:59");
        assert_eq!(format_countdown(60), "01:00");
        assert_eq!(format_countdown(3_599), "59:59");
        assert_eq!(format_countdown(3_600), "1:00:00");
        assert_eq!(format_countdown(86_399), "23:59:59");
        assert_eq!(format_countdown(86_400), "1d 00:00:00");
        assert_eq!(format_countdown(86_400 * 2 + 3_661), "2d 01:01:01");
    }

    #[test]
    fn the_config_round_trips_into_the_triggers_the_engine_speaks() {
        use crate::config::Timer;
        let t = Timer {
            enabled: true,
            mode: "countdown".into(),
            at: "07:30".into(),
            after_min: 45,
            action: "shutdown".into(),
            armed: String::new(),
        };
        assert_eq!(mode_of(&t), Mode::Countdown);
        assert_eq!(action_of(&t), Action::Shutdown);
        assert_eq!(arm_target(&t, &at_zero(2026, 9, 17, 14, 0)).unwrap().wHour, 14);
        assert_eq!(arm_target(&t, &at_zero(2026, 9, 17, 14, 0)).unwrap().wMinute, 45);

        let clock = Timer { mode: "at".into(), ..t.clone() };
        assert_eq!(mode_of(&clock), Mode::AtTime);
        // Armed for a time already past today, so tomorrow.
        let now = at_zero(2026, 9, 17, 23, 30);
        let target = arm_target(&clock, &now).unwrap();
        assert_eq!((target.wDay, target.wHour, target.wMinute), (18, 7, 30));

        // A word this version does not know is refused into the harmless
        // branch rather than guessed at: the clock, which cannot fire until a
        // time has been set, and sleep, which does not end the session.
        let unknown = Timer { mode: "later".into(), action: "hibernate".into(), ..t.clone() };
        assert_eq!(mode_of(&unknown), Mode::AtTime);
        assert_eq!(action_of(&unknown), Action::Sleep);
        // And a clock time that is not a time has no target at all, so the
        // timer cannot be armed into a state that fires the moment it is set.
        let broken = Timer { at: "soon".into(), ..clock };
        assert!(fire_at(&broken, &now).is_none());
        assert!(arm_target(&broken, &now).is_none());
    }

    #[test]
    fn a_countdown_reads_its_target_back_off_disk_rather_than_counting_again() {
        // The one thing that makes a countdown survive a restart, a reboot or a
        // sleep. Recomputing `now + 45 minutes` on load would hand out a fresh
        // full wait every time the app came back, so a 45-minute timer would
        // never fire on a machine that restarted more often than that.
        use crate::config::Timer;
        let armed_at = at_zero(2026, 9, 17, 14, 0);
        let target = after_minutes(45, &armed_at);
        let t = Timer {
            mode: "countdown".into(),
            after_min: 45,
            armed: format_stamp(&target),
            ..Timer::default()
        };

        assert_eq!(fire_at(&t, &armed_at).unwrap().wMinute, 45);
        // Two hours later it still names 14:45 — which by then is due, not
        // fifteen minutes away.
        let later = at_zero(2026, 9, 17, 16, 0);
        let when = fire_at(&t, &later).expect("the stamp is still readable");
        assert_eq!((when.wHour, when.wMinute), (14, 45));
        assert_eq!(seconds_between(&when, &later), -(2 * 60 - 45) * 60);
        // And a countdown that was never armed names nothing, which is what
        // keeps the first launch after recording a wait from firing on it.
        let never = Timer { armed: String::new(), ..t };
        assert!(fire_at(&never, &later).is_none());
    }
}
