//! User config, persisted to `%APPDATA%\ArboTray\config.json`.
//!
//! Every field is `#[serde(default)]` so an old or hand-edited config never
//! fails to load — a missing key falls back, a corrupt file is renamed aside.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A serialised `Config`, as it would appear on disk.
///
/// The Settings page has to decide whether the user actually changed anything
/// before it writes, and it can only do that from the page it is showing — so
/// it needs a copy of what `save()` would produce. Hand-rolling a second
/// default here would drift the moment a field is added.
pub const DEFAULT_JSON: &str = r##"{
  "show": {
    "net_down": true,
    "net_up": true,
    "latency": true,
    "cpu": true,
    "ram": true,
    "wifi": false,
    "usage": true,
    "sparkline": true
  },
  "interval_ms": 1000,
  "theme": {
    "foreground": "#E6E6E6",
    "background": "#000000",
    "alert": "#FF6B6B",
    "font_size": 12,
    "opacity": 0
  },
  "retention": {
    "days": 31
  },
  "quota_gb": 0.0,
  "notify": {
    "enabled": true,
    "quota_pct": 90,
    "rate_mbps": 0.0
  },
  "widget": {
    "enabled": false,
    "show": {
      "net": true,
      "latency": false,
      "hardware": true,
      "sensors": false,
      "network": false,
      "usage": false,
      "system": false
    },
    "x": null,
    "y": null,
    "always_on_top": true
  },
  "timer": {
    "enabled": false,
    "mode": "at",
    "at": "23:00",
    "after_min": 60,
    "action": "sleep",
    "armed": ""
  }
}"##;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub show: Show,
    /// Tray refresh period in milliseconds.
    pub interval_ms: u32,
    pub theme: Theme,
    pub retention: Retention,
    /// Monthly data allowance in GB. `0` means "no plan", which hides the
    /// percentage and the warning colour entirely.
    pub quota_gb: f64,
    /// Balloon notifications from the tray icon.
    pub notify: Notify,
    /// The desktop widget — the floating panel for the readings the taskbar
    /// strip has no room for. Off by default: this is the one thing the app
    /// draws that is not inside a surface Windows already owns, so it is opted
    /// into rather than started with.
    pub widget: Widget,
    /// The sleep / shut-down timer. Off by default, and *disarmed* by default
    /// even once configured — see [`Timer`].
    pub timer: Timer,
}

/// Settings for the tray balloon notification system.
///
/// Two independent gates: the data plan (announced once per threshold
/// crossing per month) and the download rate (announced on a rising edge,
/// then silenced for [`crate::taskbar::alert::RATE_COOLDOWN`]).
///
/// Both are in `config.json` so a user who never wants a notification can
/// turn the whole thing off in one edit, and a user who only wants plan alerts
/// can silence the rate side by setting `rate_mbps` to `0`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Notify {
    /// Master switch. `false` suppresses every balloon from this app.
    pub enabled: bool,
    /// Fire when the monthly total exceeds this share of the configured plan,
    /// as a whole percentage. `0` means "no rate threshold" — *not*
    /// "alert on the first byte".
    pub quota_pct: u32,
    /// Fire when the incoming rate exceeds this many MiB/s. `0.0` or any
    /// non-positive, non-finite value disables the rate alert entirely.
    pub rate_mbps: f64,
}

impl Default for Notify {
    fn default() -> Self {
        Self {
            enabled: true,
            quota_pct: 90,
            // Off by default: a rate threshold that surprised nobody on install
            // is a threshold nobody configured, and a balloon the user did not
            // ask for is the fastest way to uninstall a tray app.
            rate_mbps: 0.0,
        }
    }
}

/// The sleep / shut-down timer's settings.
///
/// The three fields that describe *what* to fire are kept apart from the one
/// that says whether it will, because they are decided at different times. What
/// and when are the settings a user picks once and leaves; `armed` is the
/// decision to let it happen tonight, made on the day — and it is deliberately
/// not remembered across a restart. A timer that survived a reboot armed would
/// shut a machine down at an hour nobody was standing in front of it to cancel.
///
/// Every field is a `String` or a plain number rather than one of the `power`
/// enums, because this is a file people hand-edit: a word this version does not
/// recognise has to land as "refused, and therefore off" rather than as a
/// deserialisation failure that takes the whole config with it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Timer {
    /// Whether the timer is switched on at all.
    pub enabled: bool,
    /// `"at"` or `"countdown"` — see `power::Mode`.
    pub mode: String,
    /// The clock time, `"HH:MM"`, for `"at"`. Local, like the clock on the wall.
    pub at: String,
    /// How many minutes from arming, for `"countdown"`.
    pub after_min: u32,
    /// `"sleep"` or `"shutdown"` — see `power::Action`.
    pub action: String,
    /// The instant a countdown was armed for, as `power::format_stamp` writes
    /// it. Empty when nothing is armed, and read back for `"countdown"` mode
    /// only: an `"at"` timer recomputes its next occurrence every time it looks.
    pub armed: String,
}

impl Default for Timer {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: "at".into(),
            // A time nobody has to act on to be safe: the default action is
            // sleep and the timer starts off, so a config written by a version
            // that did not have this section cannot put a machine to bed.
            at: "23:00".into(),
            after_min: 60,
            action: "sleep".into(),
            armed: String::new(),
        }
    }
}

/// The desktop widget's own settings.
///
/// A section of its own rather than more `show` flags, because the strip's
/// eight are one design with everything in it, while these six are another:
/// the whole point of the widget is that a reading either does not fit the
/// taskbar or is wanted bigger, and mixing the two sets into one `show` block
/// would leave no way to tell which surface a `false` was about.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Widget {
    pub enabled: bool,
    pub show: WidgetShow,
    /// Remembered top-left corner, in screen pixels. `None` until the window
    /// has been placed once, which is also how a first run knows to pick a
    /// corner of the work area rather than trusting a position it never had.
    ///
    /// `f64` rather than `i32` because this is JSON read back from a file
    /// somebody can edit, and the bounds are enforced in `clamp_x`/`clamp_y`
    /// where the work area is known — not here, where there is no screen.
    pub x: Option<f64>,
    pub y: Option<f64>,
    /// Keep the panel above other windows. Off means an ordinary window that
    /// goes behind whatever is clicked next.
    pub always_on_top: bool,
}

/// Which blocks the widget draws. Coarser than `Show`: a panel big enough to
/// hold labelled rows does not want eight independent switches for them, so
/// these group the rows the way the pages already do.
///
/// Only two of them are on by default — traffic and CPU/RAM, the readings that
/// change while you watch. The rest are page detail that happens to be reachable
/// from the desktop, and a panel with all seven blocks in it is a window, not
/// something you glance at.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WidgetShow {
    /// Download and upload rate.
    pub net: bool,
    /// Gateway and internet round-trip.
    pub latency: bool,
    /// CPU and RAM.
    pub hardware: bool,
    /// GPU, battery and power, when the machine reports them. Split out from
    /// `hardware` rather than folded in with it because these are the readings
    /// that are absent on most machines and merely interesting on the rest —
    /// keeping them here is what leaves "CPU and RAM" meaning exactly that.
    pub sensors: bool,
    /// Adapter, address, gateway, DNS, Wi-Fi.
    pub network: bool,
    /// Today, this month, and the last few days.
    pub usage: bool,
    /// Machine name, Windows build, CPU model, disks, uptime.
    pub system: bool,
}

impl Default for Widget {
    fn default() -> Self {
        Self {
            enabled: false,
            show: WidgetShow::default(),
            x: None,
            y: None,
            always_on_top: true,
        }
    }
}

impl Default for WidgetShow {
    fn default() -> Self {
        Self {
            net: true,
            latency: false,
            hardware: true,
            sensors: false,
            network: false,
            usage: false,
            system: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Show {
    pub net_down: bool,
    pub net_up: bool,
    pub latency: bool,
    pub cpu: bool,
    pub ram: bool,
    pub wifi: bool,
    /// Today's total traffic. On by default: the counter's whole job is to be
    /// visible without opening the dashboard, and it is the reading a metered
    /// connection most needs in front of it.
    pub usage: bool,
    pub sparkline: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Theme {
    /// `#RRGGBB`
    pub foreground: String,
    /// `#RRGGBB`
    pub background: String,
    /// `#RRGGBB` used for the whole run once the data plan is exceeded. The
    /// taskbar has no room for a warning icon, so colour is the whole signal.
    pub alert: String,
    pub font_size: u32,
    /// 0 = fully transparent, 255 = opaque.
    pub opacity: u8,
}

/// How much usage history to keep. One value, because there is one kind of
/// record: a day's byte total. The file this replaced described a three-tier
/// raw/minute/hour store that nothing ever built, and a field that promises
/// storage nothing writes is worse than a smaller honest one.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct Retention {
    /// Days of usage history in `usage.json`. Bounded to at least one — a
    /// window of zero would erase today's total as it was written.
    ///
    /// The default is a whole month rather than a week, because the quota is
    /// monthly: a shorter window makes `Usage::month_bytes` a sum of the days
    /// that happen to be left, which reads *low* against the plan. A week-long
    /// window would under-report the quota by roughly three quarters in the
    /// last week of the month, which is precisely when it matters most.
    pub days: u32,
}

/// The retention default before v0.11.0. Only `Config::migrate` reads it.
const OLD_DEFAULT_KEEP_DAYS: u32 = 7;

// --- bounds ---------------------------------------------------------------

/// The tray refresh period, in milliseconds, that the app will honour.
///
/// `interval_ms` can be set by hand where nothing validates it, so the bounds
/// live here and both the startup path and the Settings page read them: a
/// zero would spin a `sleep` for nothing, and ten seconds of a stale number is
/// already past the point of being a monitor.
pub fn clamp_interval_ms(ms: u32) -> u32 {
    ms.clamp(100, 10_000)
}

/// The lower bound of the quota, in GB. `0` is not merely small — it is the
/// documented "no plan" value (`Config::quota_gb`), so it has to stay
/// reachable; the Settings page offers it as its own checkbox.
pub const QUOTA_MIN_GB: f64 = 0.5;

/// Upper bound of the quota, in GB. Above this the percentage is always a
/// rounding error, and the field is only there to bound a typed number.
pub const QUOTA_MAX_GB: f64 = 100_000.0;

/// Push a quota inside its bounds, or return `None` when it is outside them.
///
/// A quota that is already out of range is left refused rather than silently
/// rounded: the file is hand-editable, so the number on screen may not be one
/// this app ever wrote, and quietly moving a user's plan is worse than saying
/// it is not usable.
pub fn clamp_quota_gb(gb: f64) -> Option<f64> {
    if gb.is_finite() && (QUOTA_MIN_GB..=QUOTA_MAX_GB).contains(&gb) {
        Some(gb)
    } else {
        None
    }
}

/// Font sizes are a typographic bound, not a data one: 9 is the smallest the
/// dashboards' own font helper will build, and 72 is the largest that leaves a
/// taskbar tile with room for more than one character.
pub fn clamp_font_size(px: u32) -> u32 {
    px.clamp(9, 72)
}

/// The refresh period as a `Duration`, clamped. The one place the sleep is
/// built, so the tracer test and the telemetry thread cannot disagree.
pub fn interval_duration(cfg: &Config) -> Duration {
    Duration::from_millis(clamp_interval_ms(cfg.interval_ms) as u64)
}

// --- defaults -------------------------------------------------------------

impl Default for Config {
    fn default() -> Self {
        Self {
            show: Show::default(),
            interval_ms: 1000,
            theme: Theme::default(),
            retention: Retention::default(),
            quota_gb: 0.0,
            notify: Notify::default(),
            widget: Widget::default(),
            timer: Timer::default(),
        }
    }
}

impl Default for Show {
    fn default() -> Self {
        Self {
            net_down: true,
            net_up: true,
            latency: true,
            cpu: true,
            ram: true,
            wifi: false,
            usage: true,
            sparkline: true,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            foreground: "#E6E6E6".into(),
            background: "#000000".into(),
            alert: "#FF6B6B".into(),
            font_size: 12,
            opacity: 0,
        }
    }
}

impl Default for Retention {
    fn default() -> Self {
        // A month, so the monthly quota can actually be measured. The cost is
        // nothing worth counting: a day's record is two integers and a date,
        // about 56 bytes, so a full window is under 2 KB of JSON.
        Self { days: 31 }
    }
}

// --- load / save ----------------------------------------------------------

impl Config {
    /// `%APPDATA%\ArboTray\config.json`
    pub fn path() -> PathBuf {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        base.join("ArboTray").join("config.json")
    }

    /// Never fails: a missing or unreadable file yields defaults.
    pub fn load() -> Self {
        let path = Self::path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match serde_json::from_str::<Self>(&text) {
            Ok(mut cfg) => {
                cfg.migrate();
                cfg
            }
            Err(_) => {
                // Keep the bad file for inspection, start clean.
                let _ = std::fs::rename(&path, path.with_extension("json.bad"));
                Self::default()
            }
        }
    }

    /// Fix up a config written by an older version.
    ///
    /// Kept separate from `load` so it can be tested without a file, and kept
    /// deliberately small: a migration list that grows without bound is how a
    /// config format becomes impossible to change.
    fn migrate(&mut self) {
        // Retention was 7 days until v0.11.0, chosen when the Data page's
        // seven-row breakdown was the only thing reading it. The monthly quota
        // now reads it too, and a week-long window makes `month_bytes` a sum of
        // whatever days are left — under-reporting the plan by roughly three
        // quarters in the last week of the month, which is when the number
        // matters most.
        //
        // Overwriting a stored value is normally wrong, but `days` has no
        // Settings row and never had one: every 7 on disk is the old default
        // rather than somebody's choice. The cost of being wrong about that is
        // under 2 KB of JSON; the cost of leaving it is a quota readout that is
        // silently low on every machine upgraded rather than freshly installed.
        if self.retention.days == OLD_DEFAULT_KEEP_DAYS {
            self.retention.days = Retention::default().days;
        }
    }

    /// This config as it would be written to disk.
    ///
    /// `serde_json` needs `Serialize` to be infallible, and `Config` is all
    /// owned primitives and `String`s, so the fallback is unreachable — it is
    /// here because `save()` has nowhere to return an error to.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| DEFAULT_JSON.into())
    }

    /// Parse a config written by `to_json`. `None` for anything unreadable,
    /// which callers treat as "not a change I can act on" rather than an error.
    /// Used by the Settings page to compare edited state, not by startup.
    pub fn from_json(text: &str) -> Option<Self> {
        serde_json::from_str(text).ok()
    }

    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&Self::path())
    }

    /// The body of `save()` against a caller-chosen path. Split out so the
    /// write path can be exercised without touching the user's real config:
    /// `save()` has exactly one path in it, and "the directory is read-only"
    /// is not a case the Settings page may get wrong.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, self.to_json())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_json_falls_back_to_defaults() {
        // The whole point of serde(default): an old config keeps loading.
        let cfg: Config = serde_json::from_str(r#"{"interval_ms": 250}"#).unwrap();
        assert_eq!(cfg.interval_ms, 250);
        assert!(cfg.show.net_down);
        assert_eq!(cfg.theme.font_size, 12);
    }

    #[test]
    fn round_trips() {
        let cfg = Config {
            interval_ms: 500,
            ..Default::default()
        };
        let back: Config = serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(back.interval_ms, 500);
    }

    #[test]
    fn the_default_page_json_is_the_default_config() {
        // The Settings page compares what it is showing against this string to
        // decide whether the user changed anything. The moment a field is added
        // to `Config` without a line here, that comparison starts lying in one
        // direction; this is the assertion that catches it.
        let from_text = Config::from_json(DEFAULT_JSON).expect("DEFAULT_JSON must parse");
        assert_eq!(
            serde_json::to_value(&from_text).unwrap(),
            serde_json::to_value(Config::default()).unwrap()
        );
    }

    #[test]
    fn to_json_round_trips_through_from_json() {
        let cfg = Config {
            interval_ms: 250,
            quota_gb: 42.5,
            show: Show {
                wifi: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let back = Config::from_json(&cfg.to_json()).expect("our own output must parse");
        assert_eq!(back.interval_ms, 250);
        assert_eq!(back.quota_gb, 42.5);
        assert!(back.show.wifi);
    }

    #[test]
    fn unparseable_text_is_not_a_config() {
        assert!(Config::from_json("").is_none());
        assert!(Config::from_json("not json at all").is_none());
    }

    #[test]
    fn saving_into_a_real_directory_writes_the_file() {
        // The one part of the write path worth proving without touching
        // `%APPDATA%`: the directory is created, and the bytes on disk parse
        // back into what was saved.
        let dir = std::env::temp_dir().join("arbotray-test-save");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join("config.json");

        let cfg = Config {
            interval_ms: 1500,
            ..Default::default()
        };
        cfg.save_to(&path).expect("temp dir is writable");
        let text = std::fs::read_to_string(&path).expect("file must exist after save");
        assert_eq!(Config::from_json(&text).unwrap().interval_ms, 1500);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_path_that_cannot_be_written_reports_an_error() {
        // Requirement 4: an unwritable target has to come back as an `Err` the
        // Settings page can show. Naming an existing *file* as the directory is
        // the portable way to make `create_dir_all` fail on every platform.
        let file = std::env::temp_dir().join("arbotray-test-not-a-dir");
        std::fs::write(&file, b"x").expect("temp dir is writable");
        let path = file.join("config.json");

        assert!(Config::default().save_to(&path).is_err());

        let _ = std::fs::remove_file(&file);
    }
}

#[cfg(test)]
mod migration_tests {
    use super::*;

    #[test]
    fn the_old_seven_day_window_is_widened_to_a_month() {
        // A config written by v0.10.0 or earlier. Left alone, its week-long
        // window would make the monthly quota read low on every upgraded
        // machine — and silently, because the number still looks plausible.
        let mut old = Config {
            retention: Retention { days: 7 },
            ..Config::default()
        };
        old.migrate();
        assert_eq!(
            old.retention.days, 31,
            "an upgraded machine must be able to see a whole month"
        );
    }

    #[test]
    fn a_window_the_user_chose_is_left_alone() {
        // Only the exact old default is touched. Anything else was either
        // hand-edited or written by a future version, and overwriting it would
        // be the migration deciding it knows better.
        for days in [1, 14, 90, 365] {
            let mut cfg = Config {
                retention: Retention { days },
                ..Config::default()
            };
            cfg.migrate();
            assert_eq!(cfg.retention.days, days, "{days} was not the old default");
        }
    }

    #[test]
    fn the_month_sum_survives_a_full_default_window() {
        // The retention default has to be at least as long as the longest
        // month, or `month_bytes` starts dropping days off the front of a month
        // still in progress — the exact failure the migration exists to stop.
        assert!(
            Retention::default().days >= 31,
            "a 31-day month must fit inside the default window"
        );
    }
}
