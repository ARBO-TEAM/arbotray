# ArboTray

Ultra-light native Windows taskbar monitor: network speed, latency, hardware usage and data-plan burn, rendered directly inside the taskbar next to the clock.

Pure Rust, native Win32 — target binary < 3 MB, no garbage collection, no console subsystem. Every feature is free; nothing is gated.

## Install

Grab the latest binary from [Releases](../../releases/latest) and run it. It is a single self-contained `.exe` — no installer, no runtime, no dependencies.

```
arbotray.exe
```

It docks itself into the taskbar immediately. To quit, right-click its tray icon (next to the clock) and choose **Exit**.

Or install it for the current user — no admin, no registry writes beyond the optional autostart entry:

```
powershell -ExecutionPolicy Bypass -File install.ps1
powershell -ExecutionPolicy Bypass -File install.ps1 -Autostart   # also start with Windows
```

### Windows Defender and SmartScreen

The release binary is **not code-signed**, and Defender scans it clean:

```
& "$env:ProgramFiles\Windows Defender\MpCmdRun.exe" -Scan -ScanType 3 -File .\arbotray.exe
# found no threats
```

What you may still see is **SmartScreen**, not a detection. A downloaded copy carries the Mark-of-the-Web, and Windows warns "Windows protected your PC" for any unsigned binary whose publisher has no download reputation yet. That is a reputation question, not a malware one. Your options:

- **Run it anyway** — click *More info* → *Run anyway*. One time per machine, per file.
- **Clear the mark before running** — right-click the `.exe` → Properties → tick *Unblock* → OK.
- **`install.ps1`** — scans with Defender first, then copies the binary into `%LOCALAPPDATA%\Programs\ArboTray\`, so nothing downloaded into a browser-tainted folder ends up on your PATH. It never disables, excludes or asks Defender to look away.
- **Sign it** — the only way to lose the prompt entirely, for anyone shipping to machines they do not control.

## Build

```
cargo build --release
```

The binary lands at `target/release/arbotray.exe` (~485 KB).

## What it shows

- **Download / upload rate** — IP Helper octet counters over the live hardware interfaces, delta per second
- **Gateway latency** — ICMP probe every 3 s, cached between probes
- **CPU and RAM** — `GetSystemTimes` and `GlobalMemoryStatusEx`
- **Wi-Fi band and signal** — WLAN API, e.g. `5G 78%`
- **Today's data usage** — running total, in the taskbar and in the icon tooltip. On by default, with no setup needed: it is the reading a metered connection most needs in front of it
- **Mini-sparkline** — recent download throughput

Hovering the tray icon shows the full readout — every enabled field under its own label, the two rates under ↓ and ↑ — plus the connected network name, which is the one field too long for the taskbar. The lines are fitted to the tooltip, so the last one is dropped rather than clipped when a machine reports everything at once.

Left-clicking the tray icon opens a dashboard: a sidebar of pages — Overview, Network, System, Data, Ports, Speed Test, Stopwatch — over the same readings, plus a chart, a built-in speed test and a stopwatch.

## Config

`%APPDATA%\ArboTray\config.json` — created on demand, loaded with defaults when missing or corrupt (a bad file is kept as `config.json.bad`).

| Key | Meaning |
| --- | --- |
| `show.*` | Which tiles to draw: `net_down`, `net_up`, `latency`, `cpu`, `ram`, `wifi`, `usage`, `sparkline` |
| `interval_ms` | Poll period, clamped to 100–10000 |
| `theme.foreground` / `background` | `#RRGGBB` |
| `theme.alert` | Colour for the whole run once the data plan is exceeded |
| `theme.font_size`, `theme.opacity` | Point size; `opacity: 0` samples the taskbar background |
| `quota_gb` | Monthly allowance. `0` disables the over-quota warning |

A malformed colour degrades to a readable default rather than taking the tray down.

## Data usage

`%APPDATA%\ArboTray\usage.json` holds today's byte total, reset when the local date changes. Totals are diffed from the raw interface counters rather than summed from the per-second rates, so they stay exact; the file is rewritten at most every 30 s, so a hard kill costs at most that much accounting.

Only the physical interfaces are counted. IP Helper also lists a row per protocol driver bound to each NIC — WFP, QoS Packet Scheduler, the Hyper-V switch extension — and every one of those rows repeats its parent NIC's counters verbatim rather than reporting its own, so a NIC with three filters bound appears four times. Summing every up row therefore multiplies the real traffic by the number of bound filters, which read as gigabytes of phantom usage from a few hundred megabytes of work.

## Start with Windows

The **Start with Windows** checkbox on the Settings page writes one value, `ArboTray`, to the per-user key Windows itself reads at logon:

```
HKCU\Software\Microsoft\Windows\CurrentVersion\Run
```

No elevation, no scheduled task — the same key `install.ps1 -Autostart` writes, so the two agree rather than fight. The path is stored quoted, because the value is parsed as a command line and an unquoted path with a space in it would be read as an executable plus arguments.

The registry is the only source of truth. There is deliberately no `autostart` field in `config.json`: a copy of that state can desync from the key — via `install.ps1 -Autostart`, Task Manager's Startup tab, or a hand edit — and the checkbox would then report a state Windows does not honour. It is for the same reason that the checkbox writes on the click instead of at Save, and rolls its tick back if the write fails.

The check compares paths rather than merely looking for the value, so a `Run` entry left pointing at a moved or reinstalled `arbotray.exe` reads as **off** — Windows would silently fail to start it, and a ticked box would be a lie about what happens at logon. Ticking it again rewrites the entry.

Starting this way means the process can be up before Explorer has created the taskbar. The app retries the attach for up to 20 seconds — the first attempt immediate, so an ordinary launch is unchanged — instead of delaying the launch from a task trigger.

## Single instance

A named mutex (`ArboTray.SingleInstance`) guards against a second copy.

## Known limits

- Taskbar children cannot be layered windows (`WS_EX_LAYERED` is refused by `Shell_TrayWnd`), so "transparent" is done by sampling the taskbar background colour — visibly wrong only over taskbar wallpapers or acrylic.
- 6 GHz Wi-Fi detection: the WLAN API exposes only a channel number, whose ranges collide between the 5 and 6 GHz bands; only channels above 177 are classified as 6 GHz.
- Data usage is machine-wide. Windows exposes no per-process byte counter without ETW, so there is no per-app breakdown.
- If Explorer restarts, the tray exits cleanly; re-attaching to the new taskbar is not implemented.

## CI

GitHub Actions on `windows-latest`:

- `ci.yml` — check, test, release build, artifact upload, on every push and PR
- `release.yml` — on a `v*` tag, tests, builds, and publishes a GitHub release with the versioned `.exe` attached
