# Blueprint Proyek: ArboTray (Rust Implementation)

**ArboTray** adalah utilitas taskbar Windows native ultra-ringan berbasis **Rust** untuk memantau kecepatan jaringan, latensi, serta penggunaan hardware secara real-time, dengan target binary < 3 MB, memory footprint < 10 MB, dan zero garbage collection overhead.

---

## 1. Arsitektur Modul & Struktur Folder

```text
arbotray/
├── Cargo.toml
├── build.rs                  # Embed Windows Application Manifest & Icon
└── src/
    ├── main.rs               # Entry point, single-instance mutex, CLI handling
    ├── app.rs                # Orchestrator & thread channel router
    ├── taskbar/              # Win32 Taskbar integration & rendering engine
    │   ├── mod.rs
    │   ├── dock.rs           # Injeksi child window ke Shell_TrayWnd
    │   ├── render.rs         # Direct2D / GDI text & mini-sparkline renderer
    │   └── events.rs         # WndProc: DPI change, TaskbarCreated, Explorer crash
    ├── telemetry/            # Data collectors (Hardware & Network)
    │   ├── mod.rs
    │   ├── network.rs        # GetIfTable2 / IP Helper API throughput
    │   ├── wifi.rs           # WLAN API (band 2.4G/5G/6G & SSID readout)
    │   ├── connections.rs    # GetExtendedTcpTable (PID connection mapping)
    │   ├── hardware.rs       # Windows PDH counters (CPU, RAM, VRAM, GPU)
    │   ├── power_temp.rs     # NVML / RAPL / LHM WMI fallback
    │   └── latency.rs        # Non-blocking ICMP ping ke gateway lokal
    ├── storage/              # Engine database lokal
    │   ├── mod.rs
    │   ├── db.rs             # SQLite connection & schema migrations
    │   └── retention.rs      # Background aggregation: raw -> minute -> hour
    ├── config/               # Manajemen konfigurasi JSON
    │   └── mod.rs            # Load/save ke %APPDATA%\ArboTray\config.json
    └── ui/                   # Window Monitor & Settings (egui/eframe)
        ├── mod.rs
        ├── overview.rs       # Live tiles, sparklines, top talkers, data usage
        ├── network_tab.rs    # Grafik bandwidth & tabel koneksi soket per-app
        ├── hardware_tab.rs   # Multi-axis CPU/GPU/RAM & tabel resource proses
        └── settings.rs       # Pengaturan tampilan, ambang batas, & kuota
