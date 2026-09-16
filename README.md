# ArboTray

Ultra-light native Windows taskbar monitor: network speed, latency, CPU and RAM, rendered directly inside the taskbar next to the clock.

Pure Rust, native Win32 — target binary < 3 MB, no garbage collection, no console subsystem.

## Build

```
cargo build --release
```

The binary lands at `target/release/arbotray.exe` (~236 KB). Run it; it docks itself into the taskbar and shows:

- download / upload rate (IP Helper octet counters, delta per second)
- gateway latency + packet loss (ICMP probe every 3 s, cached between probes)
- CPU load (`GetSystemTimes`) and RAM usage (`GlobalMemoryStatusEx`)
- optional mini-sparkline of recent download throughput

## Config

`%APPDATA%\ArboTray\config.json` — created on demand, loaded with defaults when missing or corrupt. Fields: which tiles to show, poll interval, colours (`#RRGGBB`), font size, opacity (0 = sample the taskbar background), and history retention.

## Single instance

A named mutex (`ArboTray.SingleInstance`) guards against a second copy.

## Known limits

- Taskbar children cannot be layered windows (`WS_EX_LAYERED` is refused by `Shell_TrayWnd`), so "transparent" is done by sampling the taskbar background colour — visibly wrong only over taskbar wallpapers or acrylic.
- 6 GHz Wi-Fi detection: the WLAN API exposes only a channel number, whose ranges collide between 5/6 GHz; only channels above 177 are classified as 6 GHz.
- If Explorer restarts, the tray exits cleanly and a supervisor re-attach is planned (the `TaskbarCreated` hook is in place).

## CI

GitHub Actions on `windows-latest`: check, test, release build, artifact upload.
