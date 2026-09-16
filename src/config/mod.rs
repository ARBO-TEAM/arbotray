//! User config, persisted to `%APPDATA%\ArboTray\config.json`.
//!
//! Every field is `#[serde(default)]` so an old or hand-edited config never
//! fails to load — a missing key falls back, a corrupt file is renamed aside.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub show: Show,
    /// Tray refresh period in milliseconds.
    pub interval_ms: u32,
    pub theme: Theme,
    pub retention: Retention,
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
    pub sparkline: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Theme {
    /// `#RRGGBB`
    pub foreground: String,
    /// `#RRGGBB`
    pub background: String,
    pub font_size: u32,
    /// 0 = fully transparent, 255 = opaque.
    pub opacity: u8,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct Retention {
    pub raw_days: u32,
    pub minute_days: u32,
    pub hour_days: u32,
}

// --- defaults -------------------------------------------------------------

impl Default for Config {
    fn default() -> Self {
        Self {
            show: Show::default(),
            interval_ms: 1000,
            theme: Theme::default(),
            retention: Retention::default(),
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
            sparkline: true,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            foreground: "#E6E6E6".into(),
            background: "#000000".into(),
            font_size: 12,
            opacity: 0,
        }
    }
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            raw_days: 7,
            minute_days: 30,
            hour_days: 365,
        }
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

    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into());
        std::fs::write(path, text)
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
}
