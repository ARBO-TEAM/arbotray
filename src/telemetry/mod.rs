//! Data collectors. Every collector is a struct with `new()` + `poll()`;
//! `poll()` returns `None` on failure rather than erroring, so a missing
//! sensor (no Wi-Fi card, no GPU counters) degrades one tile, not the app.
//!
//! Contract: `poll()` must never panic and must never block longer than ~100ms.

pub mod hardware;
pub mod latency;
pub mod network;
pub mod usage;
pub mod wifi;

pub use hardware::Hardware;
pub use latency::Latency;
pub use network::Network;
pub use usage::Usage;
pub use wifi::Wifi;

/// Throughput in **bytes per second**, already delta'd between polls.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NetSample {
    pub rx_bps: u64,
    pub tx_bps: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct HardwareSample {
    /// 0.0..=100.0
    pub cpu_pct: f32,
    pub ram_used_bytes: u64,
    pub ram_total_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    B2G4,
    B5G,
    B6G,
    Unknown,
}

impl Band {
    pub fn label(self) -> &'static str {
        match self {
            Band::B2G4 => "2.4G",
            Band::B5G => "5G",
            Band::B6G => "6G",
            Band::Unknown => "?",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WifiSample {
    pub ssid: Option<String>,
    pub band: Option<Band>,
    /// 0..=100
    pub signal_pct: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LatencySample {
    /// Round-trip to the default gateway.
    pub gateway_ms: Option<u32>,
    /// Round-trip to a public resolver.
    pub internet_ms: Option<u32>,
    /// 0..=100, measured over the last N probes.
    pub loss_pct: Option<u32>,
}

/// One full snapshot of everything the tray can display.
#[derive(Debug, Clone, Default)]
pub struct Metric {
    pub net: Option<NetSample>,
    pub hw: Option<HardwareSample>,
    pub wifi: Option<WifiSample>,
    pub latency: Option<LatencySample>,
}

/// Owns every collector and polls them together, in the order the tray needs.
#[derive(Default)]
pub struct Sampler {
    pub net: Network,
    pub hw: Hardware,
    pub wifi: Wifi,
    pub latency: Latency,
}

impl Sampler {
    pub fn new() -> Self {
        Self {
            net: Network::new(),
            hw: Hardware::new(),
            wifi: Wifi::new(),
            latency: Latency::new(),
        }
    }

    pub fn poll(&mut self) -> Metric {
        Metric {
            net: self.net.poll(),
            hw: self.hw.poll(),
            wifi: self.wifi.poll(),
            latency: self.latency.poll(),
        }
    }
}
