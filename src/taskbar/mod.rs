//! Taskbar integration. Owns the Win32 child window docked inside
//! `Shell_TrayWnd`, and paints the metric text into it.
//!
//! `dock` finds and attaches to the taskbar, `render` draws, `events` is the
//! WndProc that follows DPI changes. An Explorer restart destroys the parent
//! taskbar and with it our child window — see the `TaskbarCreated` arm in
//! `events` for why that currently ends the process instead of re-attaching.

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

    // --- the System page's detail ------------------------------------------
    //
    // Like the network detail above, none of these is gated on `cfg.show` and
    // none of them reaches `visible_segments`. The taskbar has no room for a
    // machine name, and a strip padded with the Windows build number is a strip
    // nobody would keep switched on.

    /// The machine's name on the network.
    pub computer_text: String,
    /// Edition, release and build.
    pub windows_text: String,
    /// The processor's marketing name.
    pub cpu_name_text: String,
    /// `6 cores / 12 threads`, or whichever half this machine will admit to.
    pub cores_text: String,
    /// Display adapters, joined. Already de-duplicated by the collector.
    pub gpu_text: String,
    /// Charge left. Blank on a machine with no battery.
    pub battery_text: String,
    /// `Plugged in` or `On battery`.
    pub power_text: String,
    /// `3d 4h`, at two units of precision.
    pub uptime_text: String,
    /// One entry per local volume, as `(mount, "210G free of 931G")`.
    ///
    /// A list of label-value pairs rather than one joined string, because a
    /// drive letter belongs in a row's *label* and its usage in the value — and
    /// a machine with three volumes joined into one row would be a wall of text
    /// ellipsised at the first drive. Declared here rather than left in the
    /// sample because the formatted pair is what the page draws, and formatting
    /// is what the model is for.
    pub disks: Vec<(String, String)>,

    // --- the Ports page's detail -------------------------------------------
    //
    // Page detail like the rest: no port figure has a tile, and none of these
    // appears in `visible_segments`, so a machine with two hundred open sockets
    // has exactly the taskbar its owner asked for.

    /// TCP sockets in LISTEN — the machine's open doors.
    pub listeners_text: String,
    /// TCP sockets with a peer.
    pub established_text: String,
    /// Bound UDP sockets.
    pub udp_text: String,
    /// Distinct processes holding any of the above.
    pub port_owners_text: String,
    /// The listening ports worth naming.
    ///
    /// A list rather than a joined string for the same reason `disks` is: the
    /// port belongs in a row's label and its owner in the value, and a machine
    /// with a dozen listeners joined into one row would be a paragraph
    /// ellipsised at the first port.
    pub open_ports: Vec<OpenPort>,

    // --- the Speed Test page's detail --------------------------------------

    /// What the test is doing: a phase label, or empty when idle.
    pub speed_phase_text: String,
    /// 0..=100 through the current run, for the progress readout.
    pub speed_percent: u32,
    /// Whether a run is in flight. Carried as a flag rather than read back off
    /// the phase label, because the label is for the reader and this is for the
    /// button: a string comparison would make the button's enablement depend on
    /// how a phase was spelled.
    pub speed_running: bool,
    /// The run in flight, or the last one to finish.
    pub speed_down_text: String,
    pub speed_up_text: String,
    pub speed_latency_text: String,
    /// Why the last run stopped early.
    pub speed_error_text: String,
    /// When the last run finished, e.g. `14:32:07`, so a stale result is not
    /// mistaken for a live one.
    pub speed_when_text: String,
    /// Completed runs, newest first, as `(time, "down / up")`.
    pub speed_history: Vec<(String, String)>,

    /// Recent download throughput, oldest first — the mini-sparkline source.
    pub history: Vec<u64>,
}

/// One row of the Ports page: a port, who holds it, and the pid to stop it by.
///
/// A named struct rather than a third tuple element because the pid is not part
/// of what the row *says* — it is what a destructive action on the row is aimed
/// at. Reading the two apart is the difference between drawing a row and ending
/// a process, and a `.2` at the call site would hide that.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenPort {
    /// The row's label, e.g. `:445`.
    pub port: String,
    /// The row's value: the program, and its pid after it.
    pub owner: String,
    /// The process holding the socket — what `ports::stop_process` takes.
    pub pid: u32,
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
        // The machine's identity. Always page detail, never a tile: there is no
        // checkbox for any of it, and `visible_segments` names its fields
        // explicitly, so none of these can leak onto the strip.
        let sys = &m.system;
        out.computer_text = sys.computer_name.clone().unwrap_or_default();
        out.windows_text = sys.windows.clone().unwrap_or_default();
        out.cpu_name_text = sys.cpu_name.clone().unwrap_or_default();
        out.cores_text = format_cores(sys.physical_cores, sys.logical_cores);
        out.gpu_text = sys.gpus.join(", ");
        out.battery_text = sys.battery_pct.map(|p| format!("{p}%")).unwrap_or_default();
        // Plugged and unplugged are worth saying even where a battery is not
        // there to report a level: a desktop is permanently on mains, and that
        // is the answer to the same question.
        out.power_text = match sys.on_ac {
            Some(true) => "Plugged in".into(),
            Some(false) => "On battery".into(),
            None => String::new(),
        };
        out.uptime_text = sys.uptime_secs.map(format_uptime).unwrap_or_default();
        out.disks = sys
            .disks
            .iter()
            .map(|d| {
                (
                    d.mount.clone(),
                    format!(
                        "{} free of {}",
                        crate::telemetry::usage::format_size(d.free_bytes),
                        crate::telemetry::usage::format_size(d.total_bytes)
                    ),
                )
            })
            .collect();

        // The port tables. Always present in the sample, so the counts always
        // draw — a machine with nothing open reads `0`, which is an answer.
        let ports = &m.ports;
        out.listeners_text = ports.listeners.to_string();
        out.established_text = ports.established.to_string();
        out.udp_text = ports.udp.to_string();
        out.port_owners_text = ports.owners.to_string();
        out.open_ports = ports
            .open
            .iter()
            .map(|p| {
                // The program's name where it could be read, its pid where it
                // could not: the pid is always right, and a row reading
                // `:445  System  pid 4` is more use than one reading
                // `:445  —`.
                let owner = match &p.process {
                    Some(name) => format!("{name}  pid {}", p.pid),
                    None => format!("pid {}", p.pid),
                };
                OpenPort {
                    port: format!(":{}", p.port),
                    owner,
                    pid: p.pid,
                }
            })
            .collect();

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

    /// Fold a speed-test run into the page's fields.
    ///
    /// A method rather than a branch of `from_metric`, because unlike every
    /// other field this one is *stateful*: a run outlives the sample it was
    /// started in, and its history would be lost if it were rebuilt from a
    /// metric each tick. `from_metric` stays pure and this stays the one place
    /// a running test is rendered, which is the same split `usage_text` makes.
    pub fn set_speed(&mut self, status: &crate::telemetry::speedtest::SpeedStatus) {
        self.speed_phase_text = status.phase.label().to_string();
        self.speed_percent = status.percent;
        self.speed_running = status.running();
        // Dashed rather than blank for a measurement that has not happened: the
        // page has a row either way, and an empty value reads as a rendering
        // fault next to a label.
        self.speed_down_text = match status.down_bps {
            Some(bps) => format_rate(bps),
            None => "--".into(),
        };
        self.speed_up_text = match status.up_bps {
            Some(bps) => format_rate(bps),
            None => "--".into(),
        };
        self.speed_latency_text = match status.latency_ms {
            Some(ms) => format!("{ms}ms"),
            None => "--".into(),
        };
        self.speed_error_text = status.error.clone().unwrap_or_default();
        self.speed_when_text = status.finished_at.clone().unwrap_or_default();
        self.speed_history = status
            .history
            .iter()
            .enumerate()
            .map(|(i, r)| {
                // The run's own stamp where the run recorded one. Only the
                // newest has a time stored, so older rows are numbered — a
                // number is honest, where repeating the newest run's time
                // against four other rows would not be.
                let when = match (i, &status.finished_at) {
                    (0, Some(t)) => t.clone(),
                    _ => format!("#{}", i + 1),
                };
                (
                    when,
                    format!(
                        "{} / {}",
                        format_rate(r.down_bps),
                        format_rate(r.up_bps)
                    ),
                )
            })
            .collect();
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

/// `6 cores / 12 threads`, from whichever of the two counts the machine gave.
///
/// Both halves are printed rather than one number because the pair is the
/// reading: 6 and 12 says SMT is on, 6 and 6 says it is not, and either number
/// alone cannot tell you which machine you are on. A machine that answers
/// neither half produces an empty row rather than a zero.
///
/// Singular forms are spelled out: "1 cores" is the kind of detail that makes a
/// whole page look unfinished.
pub fn format_cores(physical: Option<u32>, logical: Option<u32>) -> String {
    let cores = physical.map(|c| match c {
        1 => "1 core".to_string(),
        n => format!("{n} cores"),
    });
    let threads = logical.map(|t| match t {
        1 => "1 thread".to_string(),
        n => format!("{n} threads"),
    });
    match (cores, threads) {
        (Some(c), Some(t)) => format!("{c} / {t}"),
        (Some(c), None) => c,
        (None, Some(t)) => t,
        (None, None) => String::new(),
    }
}

/// `3d 4h`, `4h 12m`, `12m` — two units, the largest that apply.
///
/// Two and not three: the seconds are noise on a row that answers "did this
/// machine just boot?", and a lifetime printed to the second is a lifetime
/// nobody reads. Under a minute says `just now`, because `0m` looks like a
/// reading that failed rather than one that is simply small.
pub fn format_uptime(secs: u64) -> String {
    let minutes = secs / 60;
    let hours = minutes / 60;
    let days = hours / 24;
    if days > 0 {
        format!("{days}d {}h", hours % 24)
    } else if hours > 0 {
        format!("{hours}h {}m", minutes % 60)
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        "just now".to_string()
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
            system: Default::default(),
            // Nothing listening is the case this test is not about; the
            // struct-update keeps it from having to say so field by field.
            ..Default::default()
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
