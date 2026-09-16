//! Data collectors. Every collector is a struct with `new()` + `poll()`;
//! `poll()` returns `None` on failure rather than erroring, so a missing
//! sensor (no Wi-Fi card, no GPU counters) degrades one tile, not the app.
//!
//! Contract: `poll()` must never panic and must never block longer than ~100ms.

pub mod adapter;
pub mod hardware;
pub mod latency;
pub mod network;
pub mod system;
pub mod usage;
pub mod wifi;

pub use adapter::Adapter;
pub use hardware::Hardware;
pub use latency::Latency;
pub use network::Network;
pub use system::SystemInfo;
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
    /// The default gateway itself, in network byte order — its *bytes* are the
    /// address, its numeric value is not. Format it through
    /// `TrayModel::format_addr`, never with `Ipv4Addr::from(u32)`, which reads
    /// the value big-endian and prints `192.168.1.1` as `1.1.168.192`. Knowing
    /// which router answered is what makes `gateway_ms` a reading.
    pub gateway_addr: Option<u32>,
    /// Round-trip to the default gateway.
    pub gateway_ms: Option<u32>,
    /// Round-trip to a public resolver.
    pub internet_ms: Option<u32>,
    /// 0..=100, measured over the last N probes.
    pub loss_pct: Option<u32>,
}

/// The interface the traffic is flowing through, as Windows describes it.
///
/// A separate sample from `NetSample` rather than more fields on it: throughput
/// is a rate that changes every tick, this is configuration that changes when
/// the network does, and `network` sums *every* interface where this names one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdapterSample {
    /// The friendly name Windows shows in its own UI, e.g. `Wi-Fi`.
    pub name: Option<String>,
    /// The driver's description of the card, e.g. the vendor's long model name.
    pub description: Option<String>,
    /// IPv4 address, in network byte order like every other Win32 address —
    /// its bytes are the address. Format it through
    /// `crate::taskbar::format_addr`, never `Ipv4Addr::from(u32)`.
    pub local_addr: Option<u32>,
    /// Resolvers in the order Windows tries them.
    pub dns: Vec<u32>,
}

/// The machine itself: what it is, and what its power is doing.
///
/// Every field is optional because every one of them can genuinely be missing —
/// a desktop has no battery, a locked-down machine has no registry to read a CPU
/// name out of. `None` is "this machine cannot answer", and the System page
/// drops the row rather than printing a placeholder for it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemSample {
    /// The machine's name on the network.
    pub computer_name: Option<String>,
    /// Edition, release and build, e.g. `Windows 11 Pro 24H2 (build 26100.3915)`.
    pub windows: Option<String>,
    /// The processor's marketing name, not its architecture.
    pub cpu_name: Option<String>,
    /// Physical cores, from the platform's relation table.
    pub physical_cores: Option<u32>,
    /// Logical processors Windows will schedule on — threads, not cores. Kept
    /// beside the physical count because the pair is the informative reading:
    /// 6 and 12 says SMT is on, 6 and 6 says it is not.
    pub logical_cores: Option<u32>,
    /// Display adapters, in enumeration order, de-duplicated.
    pub gpus: Vec<String>,
    /// Charge left, 0..=100. `None` on a machine with no battery.
    pub battery_pct: Option<u32>,
    /// On mains power. `None` when Windows will not say, which is neither.
    pub on_ac: Option<bool>,
    /// Seconds since boot.
    pub uptime_secs: Option<u64>,
}

/// One full snapshot of everything the tray can display.
#[derive(Debug, Clone, Default)]
pub struct Metric {
    pub net: Option<NetSample>,
    pub hw: Option<HardwareSample>,
    pub wifi: Option<WifiSample>,
    pub latency: Option<LatencySample>,
    pub adapter: Option<AdapterSample>,
    /// Not optional, unlike its neighbours: its collector answers on any running
    /// machine, and the fields that can genuinely be missing are `Option`s
    /// inside it.
    pub system: SystemSample,
}

/// Owns every collector and polls them together, in the order the tray needs.
#[derive(Default)]
pub struct Sampler {
    pub net: Network,
    pub hw: Hardware,
    pub wifi: Wifi,
    pub latency: Latency,
    pub adapter: Adapter,
    pub system: SystemInfo,
}

impl Sampler {
    pub fn new() -> Self {
        Self {
            net: Network::new(),
            hw: Hardware::new(),
            wifi: Wifi::new(),
            latency: Latency::new(),
            adapter: Adapter::new(),
            system: SystemInfo::new(),
        }
    }

    pub fn poll(&mut self) -> Metric {
        Metric {
            net: self.net.poll(),
            hw: self.hw.poll(),
            wifi: self.wifi.poll(),
            latency: self.latency.poll(),
            adapter: self.adapter.poll(),
            system: self.system.poll(),
        }
    }
}
