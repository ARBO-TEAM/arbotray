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
    "days": 7
  },
  "quota_gb": 0.0
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
    pub days: u32,
}

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
        Self { days: 7 }
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
        match serde_json::from_str(&text) {
            Ok(cfg) => cfg,
            Err(_) => {
                // Keep the bad file for inspection, start clean.
                let _ = std::fs::rename(&path, path.with_extension("json.bad"));
                Self::default()
            }
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
