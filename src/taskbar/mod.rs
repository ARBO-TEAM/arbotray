//! Taskbar integration. Owns the Win32 child window docked inside
//! `Shell_TrayWnd`, and paints the metric text into it.
//!
//! `dock` finds and attaches to the taskbar, `render` draws, `events` is the
//! WndProc that survives Explorer restarts and DPI changes.

pub mod dock;
pub mod events;
pub mod render;

pub use dock::{Notifier, Tray};

use crate::config::Config;
use crate::telemetry::Metric;

/// What the renderer is asked to draw. Kept separate from `Metric` so the
/// renderer never has to know about Win32 types and can be unit-tested.
#[derive(Debug, Clone, Default)]
pub struct TrayModel {
    pub down_text: String,
    pub up_text: String,
    pub latency_text: String,
    pub cpu_text: String,
    pub ram_text: String,
    /// Recent download throughput, oldest first — the mini-sparkline source.
    pub history: Vec<u64>,
}

impl TrayModel {
    /// Formats one sample using the user's visibility settings.
    /// Pure function — this is the piece worth testing.
    pub fn from_metric(m: &Metric, cfg: &Config) -> Self {
        let mut out = TrayModel::default();

        if let Some(n) = &m.net {
            if cfg.show.net_down {
                out.down_text = format_rate(n.rx_bps);
            }
            if cfg.show.net_up {
                out.up_text = format_rate(n.tx_bps);
            }
        }
        if cfg.show.latency {
            out.latency_text = match m.latency.as_ref().and_then(|l| l.gateway_ms) {
                Some(ms) => format!("{ms}ms"),
                None => "--".into(),
            };
        }
        if let Some(hw) = &m.hw {
            if cfg.show.cpu {
                out.cpu_text = format!("{:.0}%", hw.cpu_pct);
            }
            if cfg.show.ram {
                let pct = if hw.ram_total_bytes > 0 {
                    hw.ram_used_bytes as f32 / hw.ram_total_bytes as f32 * 100.0
                } else {
                    0.0
                };
                out.ram_text = format!("{pct:.0}%");
            }
        }
        out
    }
}

/// Human-friendly rate. Picks one unit and stays in it so the tray text
/// doesn't jitter between columns while numbers are small.
pub fn format_rate(bps: u64) -> String {
    const KIB: f64 = 1024.0;
    let b = bps as f64;
    if b < KIB {
        format!("{bps}B/s")
    } else if b < KIB * KIB {
        format!("{:.1}K/s", b / KIB)
    } else if b < KIB * KIB * KIB {
        format!("{:.1}M/s", b / (KIB * KIB))
    } else {
        format!("{:.2}G/s", b / (KIB * KIB * KIB))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::{HardwareSample, LatencySample, NetSample};

    #[test]
    fn rates_pick_a_sane_unit() {
        assert_eq!(format_rate(0), "0B/s");
        assert_eq!(format_rate(512), "512B/s");
        assert_eq!(format_rate(2048), "2.0K/s");
        assert_eq!(format_rate(5 * 1024 * 1024), "5.0M/s");
        assert_eq!(format_rate(3 * 1024 * 1024 * 1024), "3.00G/s");
    }

    #[test]
    fn hidden_metrics_stay_blank() {
        let mut cfg = Config::default();
        cfg.show.net_down = false;
        cfg.show.cpu = false;
        let m = Metric {
            net: Some(NetSample {
                rx_bps: 1024,
                tx_bps: 2048,
            }),
            hw: Some(HardwareSample {
                cpu_pct: 42.0,
                ram_used_bytes: 4,
                ram_total_bytes: 8,
            }),
            latency: Some(LatencySample {
                gateway_ms: Some(7),
                ..Default::default()
            }),
            wifi: None,
        };
        let model = TrayModel::from_metric(&m, &cfg);
        assert_eq!(model.down_text, "");
        assert_eq!(model.cpu_text, "");
        assert_eq!(model.up_text, "2.0K/s");
        assert_eq!(model.latency_text, "7ms");
        assert_eq!(model.ram_text, "50%");
    }

    #[test]
    fn missing_latency_renders_placeholder() {
        let cfg = Config::default();
        let m = Metric::default();
        assert_eq!(TrayModel::from_metric(&m, &cfg).latency_text, "--");
    }
}
