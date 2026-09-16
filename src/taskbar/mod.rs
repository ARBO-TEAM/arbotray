//! Taskbar integration. Owns the Win32 child window docked inside
//! `Shell_TrayWnd`, and paints the metric text into it.
//!
//! `dock` finds and attaches to the taskbar, `render` draws, `events` is the
//! WndProc that survives Explorer restarts and DPI changes.

pub mod dock;
pub mod events;
pub mod icon;
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
    /// The router itself, e.g. `192.168.1.1`. Shown next to the gateway
    /// latency so the number has a subject.
    pub gateway_text: String,
    /// Round-trip to a public resolver — the other half of "is it me or my
    /// provider?". Separate from `latency_text`, which is the gateway.
    pub internet_text: String,
    /// Probe loss over the last 10 pings, e.g. `10%`. Blank until a probe has
    /// landed: 0% of nothing is unknown, not perfect.
    pub loss_text: String,
    /// Band and signal, e.g. `5G 78%`. The SSID is too long for the taskbar
    /// and lives in the icon's hover tooltip instead.
    pub wifi_text: String,
    /// SSID of the connected network, for the tooltip only.
    pub wifi_name: Option<String>,
    /// What to call the interface the traffic leaves by, e.g. `Wi-Fi`. Page
    /// detail: the address below needs a subject, or "192.168.1.10" is a
    /// number with nothing attached to it.
    pub adapter_text: String,
    /// The address *this* machine holds on that interface. Not to be confused
    /// with `gateway_text`, which is the router's: a page that shows only one of
    /// them leaves the reader guessing which end of the cable it is.
    pub ip_text: String,
    /// Every resolver Windows was handed, in the order it tries them. All of
    /// them rather than the first: a dead primary behind a working secondary is
    /// a real configuration, and this page is where that is visible.
    pub dns_text: String,
    /// Today's total traffic, e.g. `1.4G`. Filled by the sampler loop rather
    /// than `from_metric`: the usage counter is stateful, and `from_metric`
    /// stays pure so it remains testable.
    pub usage_text: String,
    /// Today's total is over the configured plan. Drives a colour swap, since
    /// text in the taskbar has no room for an icon.
    pub quota_alert: bool,
    /// Traffic over the current month, e.g. `41.2G`. Page detail: the strip
    /// shows today, and today alone cannot answer "is this month unusual?".
    pub month_text: String,
    /// The month total sums only the days the file still holds, so it is a
    /// partial month. Said in the row's *label* rather than in the number,
    /// because a value the reader has to decode twice is a value they misread.
    pub month_partial: bool,
    /// The last few days, oldest first, as `(MM-DD, total)`. The Data page
    /// draws these; the strip never does. Amended by reference to `Usage`
    /// rather than as a rendered string because each row is its own label.
    pub usage_days: Vec<(String, u64)>,
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
        // Not gated on `cfg.show`: these three are detail for the Network page
        // and deliberately have no tile of their own. They are absent from
        // `render::visible_segments`, so they cannot reach the strip or the
        // tooltip — the taskbar stays as wide as the user asked for, and the
        // adapter detail stays one click away where it belongs.
        if let Some(l) = &m.latency {
            out.gateway_text = l.gateway_addr.map(format_addr).unwrap_or_default();
            // Dashed rather than blank when the internet is unreachable:
            // "we could not measure this" and "not switched on" are different
            // answers and must not look the same.
            out.internet_text = match l.internet_ms {
                Some(ms) => format!("{ms}ms"),
                None => "--".into(),
            };
            out.loss_text = l.loss_pct.map(|pct| format!("{pct}%")).unwrap_or_default();
        }
        // Also page detail, also absent from `visible_segments`: whose interface
        // this is, and what address it holds. Each field is formatted only when
        // the collector answered it, so a machine that reports an adapter but no
        // IPv4 shows the adapter and leaves the address row empty.
        if let Some(a) = &m.adapter {
            out.adapter_text = a
                .name
                .clone()
                .or_else(|| a.description.clone())
                .unwrap_or_default();
            out.ip_text = a.local_addr.map(format_addr).unwrap_or_default();
            out.dns_text = a
                .dns
                .iter()
                .map(|&addr| format_addr(addr))
                .collect::<Vec<_>>()
                .join(", ");
        }
        if cfg.show.wifi {
            // No adapter (or not on Wi-Fi) stays blank rather than showing a
            // permanent "?" — an indicator that is always there and always
            // meaningless is just noise in the taskbar.
            if let Some(w) = &m.wifi {
                let band = w.band.as_ref().map(|b| b.label()).unwrap_or("?");
                out.wifi_text = match w.signal_pct {
                    Some(pct) => format!("{band} {pct}%"),
                    None => band.to_string(),
                };
                out.wifi_name = w.ssid.clone();
            }
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

    /// Hover text for the tray icon.
    ///
    /// The topic is the one thing the taskbar has no room for: the network
    /// name. `szTip` renders newlines as line breaks and has no other
    /// formatting, and the buffer is fixed-size, so the caller truncates.
    pub fn tooltip(&self) -> String {
        let mut tip = String::from("ArboTray");
        let metrics = render::visible_segments(self).join("  ");
        if !metrics.is_empty() {
            tip.push('\n');
            tip.push_str(&metrics);
        }
        if let Some(name) = &self.wifi_name {
            tip.push_str("\nWiFi: ");
            tip.push_str(name);
        }
        tip
    }
}

/// Dotted-quad for an address that arrived from Win32 as a `u32`.
///
/// The value is in network byte order, so its **bytes** are the address and its
/// numeric value is not. `Ipv4Addr::from(u32)` would read the value big-endian
/// and print this machine's `192.168.1.1` as `1.1.168.192`; the bytes are the
/// only correct route. Locked by `addresses_are_read_in_network_byte_order`.
pub fn format_addr(addr: u32) -> String {
    std::net::Ipv4Addr::from(addr.to_ne_bytes()).to_string()
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
    use crate::telemetry::{AdapterSample, HardwareSample, LatencySample, NetSample};

    /// The exact `S_addr` this machine's router returns for the value
    /// `ipconfig` prints as `192.168.1.1`. If someone "simplifies"
    /// `format_addr` back to `Ipv4Addr::from(addr)` the gateway reads
    /// `1.1.168.192` and this fails.
    #[test]
    fn addresses_are_read_in_network_byte_order() {
        assert_eq!(format_addr(0x0101_a8c0), "192.168.1.1");
        assert_eq!(format_addr(0), "0.0.0.0");
    }

    #[test]
    fn unreachable_internet_is_dashed_not_blank() {
        // These three are page detail with no tile of their own, so the
        // default config is what a user actually has.
        let cfg = Config::default();
        let m = Metric {
            latency: Some(LatencySample {
                gateway_addr: Some(0x0101_a8c0),
                gateway_ms: Some(4),
                internet_ms: None,
                loss_pct: None,
            }),
            ..Default::default()
        };
        let model = TrayModel::from_metric(&m, &cfg);
        assert_eq!(model.gateway_text, "192.168.1.1");
        assert_eq!(model.internet_text, "--");
        // No probe has landed yet, so loss is unknown — not 0%.
        assert_eq!(model.loss_text, "");
    }

    /// The same byte-order rule the gateway test pins, one layer out: these
    /// addresses come from a `SOCKADDR` rather than a `u32` field, and a wrong
    /// direction there is just as invisible on the happy path.
    #[test]
    fn adapter_addresses_are_formatted_in_network_byte_order() {
        let cfg = Config::default();
        let m = Metric {
            adapter: Some(AdapterSample {
                name: Some("Wi-Fi".into()),
                description: Some("Intel(R) Wi-Fi 6 AX201 160MHz".into()),
                // What the stack reports for `ipconfig`'s 192.168.1.10.
                local_addr: Some(0x0a01_a8c0),
                dns: vec![0x0101_a8c0, 0x0808_0808],
            }),
            ..Default::default()
        };
        let model = TrayModel::from_metric(&m, &cfg);
        assert_eq!(model.adapter_text, "Wi-Fi");
        assert_eq!(model.ip_text, "192.168.1.10");
        // All of them, in the order Windows tries them — a dead primary behind
        // a working secondary is exactly what this row is for.
        assert_eq!(model.dns_text, "192.168.1.1, 8.8.8.8");
    }

    #[test]
    fn an_adapter_without_a_friendly_name_uses_its_description() {
        // The friendly name is the one Windows' own UI shows and is what a user
        // will recognise, but it is not guaranteed to be there.
        let m = Metric {
            adapter: Some(AdapterSample {
                name: None,
                description: Some("Realtek PCIe GbE Family Controller".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let model = TrayModel::from_metric(&m, &Config::default());
        assert_eq!(model.adapter_text, "Realtek PCIe GbE Family Controller");
    }

    #[test]
    fn no_adapter_and_no_addresses_leave_the_detail_blank() {
        // No adapter at all, then an adapter that answered with nothing: both
        // have to produce empty strings, because the page drops empty rows and
        // a bare ", " of joined nothing is not an empty string.
        let bare = TrayModel::from_metric(&Metric::default(), &Config::default());
        assert_eq!(bare.adapter_text, "");
        assert_eq!(bare.ip_text, "");
        assert_eq!(bare.dns_text, "");

        let empty = Metric {
            adapter: Some(AdapterSample::default()),
            ..Default::default()
        };
        let model = TrayModel::from_metric(&empty, &Config::default());
        assert_eq!(model.adapter_text, "");
        assert_eq!(model.ip_text, "");
        assert_eq!(model.dns_text, "");
    }

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
            adapter: None,
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
