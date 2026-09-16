# ArboTray Feature Status

Audit of the feature list in `docs/IMPROVE.md` against the source tree of **arbotray v0.2.0** (`Cargo.toml:3`) — 2026-09-16.

Every row below was verified against working code. A config field, a struct field or a TODO comment is not counted as a feature: **Done** means a user can see or use it today.

## Summary

| Feature | Status | Evidence |
| --- | --- | --- |
| Live Speed Widget | Done | `src/taskbar/mod.rs:50-54` formats `rx_bps`/`tx_bps` into `down_text`/`up_text`; painted at `src/taskbar/render.rs:243-253` |
| Always-on-Top Widget | Not done | No `HWND_TOPMOST` / `WS_EX_TOPMOST` anywhere; the only `SetWindowPos` is sidebar layout with `SWP_NOZORDER` (`src/ui/mod.rs:533-541`) |
| System Tray Icon | Partial | Icon installed and tooltip updated (`src/taskbar/icon.rs:72-104`), but no `NIF_INFO` / balloon → no notifications |
| Settings (UI) | Not done | `Config::save()` exists (`src/config/mod.rs:138-145`) with **zero callers**; no settings UI of any kind |
| Adapter Config | Not done | No `GetAdaptersAddresses` / `IP_ADAPTER_*`; interface selection never exists (`src/telemetry/network.rs:32-54` sums all adapters) |
| Dark & Light Theme | Partial | Colours/font/alert fully applied via config JSON (`src/taskbar/render.rs:222-237`, `src/ui/mod.rs:569-574`); no theme picker and `Theme.opacity` is only tested as `== 0` |
| Dashboard | Partial | Real window with 4-page sidebar (`src/ui/mod.rs:56-60`, `page_rows` at `963-1006` in-file: `:244-279`) but only shows the 8 taskbar values; no charts/statistics |
| Data Plan | Partial | Quota percent + over-quota colour works (`src/app.rs:79-85`, `src/telemetry/usage.rs:145-150`); quota is set by hand-editing JSON and the percentage is never printed |
| WiFi | Partial | SSID, band and signal read (`src/telemetry/wifi.rs:101-111`); no signal history, no saved-password management, no scan |
| Network Tools | Not done | No traceroute, DNS check or connection analysis; the only ICMP is a fixed gateway probe |
| Speed Test | Not done | Zero hits for `speed_test` / `SpeedTest` / `download_test`; no on-demand throughput measurement |
| Test History | Not done | No test-result model, no persisted history; `src/telemetry/usage.rs` stores one day's counters only |
| Usage Stats | Partial | Daily total only (`src/telemetry/usage.rs:36-150`); `Retention` (`src/config/mod.rs:52-58`) is parsed but never read → no daily/monthly breakdown |
| Network Info | Partial | Gateway latency (`src/telemetry/latency.rs:53-90`) and SSID (`wifi.rs:135-164`) reach the UI; gateway IP, local IP, DNS and `loss_pct` never do |
| Network Interface | Not done | `GetIfTable2` rows are summed and discarded (`src/telemetry/network.rs:44-50`); no per-interface stats |
| Active Process | Not done | No `GetProcessIoCounters` / ETW / PID mapping; README documents this as unavailable |
| Stopwatch | Not done | Zero hits for `stopwatch`; no session timer anywhere |
| Port Active | Not done | No `GetExtendedTcpTable` / `GetTcpTable` / `GetUdpTable` |

Counts: **4 Done, 6 Partial, 10 Not done** (20 items).

The four dashboard pages (`src/ui/mod.rs:56`) are `Overview` (0), `Network` (1), `System` (2), `Data` (3). A fifth page (e.g. `Settings`) has to be appended to `PAGES`, not inserted — the `OVERVIEW`/`NETWORK`/`SYSTEM`/`DATA` constants (`src/ui/mod.rs:57-60`) are positional.

## Done

### Live Speed Widget
The product's core and complete. `Sampler::poll` reads cumulative octet counters over every up, non-loopback interface and divides the delta by elapsed time (`src/telemetry/network.rs:21-26`, `:72-89`); `TrayModel::from_metric` formats both directions (`src/taskbar/mod.rs:48-55`); the renderer draws them into the taskbar next to the clock (`src/taskbar/render.rs:240-253`), with a 60-sample download sparkline (`src/app.rs:17-18`, `:87-97`). Counter resets are handled as lost deltas rather than spikes (`network.rs:111-115`).

### Network Info — gateway latency (the part that ships)
`IcmpSendEcho` against the default route, probed every 3 s and cached between polls so the tray never stalls (`src/telemetry/latency.rs:20-25`, `:160-186`), rendered as `NNms` with a `--` placeholder when unreachable (`src/taskbar/mod.rs:56-61`).

### Usage total + quota alert (the part that ships)
Today's bytes are accumulated from raw counters, not from rounded rates, persisted to `%APPDATA%\ArboTray\usage.json` at most every 30 s, and rolled over at local midnight (`src/telemetry/usage.rs:78-123`). Over-quota recolours the whole taskbar run and the dashboard (model flag set at `src/app.rs:79-85`; consumed at `src/taskbar/render.rs:233-237` and `src/ui/mod.rs:570-574`).

### Wi-Fi readout (the part that ships)
WLAN API handle opened once and closed on drop; SSID decoded lossily from the raw 32-byte payload with a bounded length (`src/telemetry/wifi.rs:42-53`), band derived from the channel number (`:31-38`), signal quality 0-100. Shown as `5G 78%` in the taskbar when `show.wifi` is on, and the SSID in the icon tooltip and on the Network page (`src/taskbar/mod.rs:96-108`, `src/ui/mod.rs:252-260`).

## Partial

### System Tray Icon — no notifications
Icon, tooltip and the right-click menu are complete, including the drop-time `NIM_DELETE` that prevents ghost icons (`src/taskbar/icon.rs:86-114`) and the `Write_tip`-style truncating copy that cannot overrun `szTip` (`:155-163`). What is missing vs IMPROVE.md is "notifikasi langsung dari area system tray": there is no `NIF_INFO`, no balloon, no toast. The icon reflects live speed in its hover text (`src/taskbar/events.rs:86-88`) but never pushes an alert.

- Next step: on the existing `WM_TRAY_UPDATE` path, when `quota_alert` flips false→true, call `Shell_NotifyIconW(NIM_MODIFY)` with `uFlags |= NIF_INFO` and a fixed `szInfo`/`szInfoTitle`. Reuse `WindowState.model` (`events.rs:83-88`) to detect the edge; do not add a timer.
- Dashboard page: none — tray-level.

### Dark & Light Theme — no picker, and light mode has never been exercised
Config-driven colours, font size and the over-quota alert colour are all wired end to end (`src/taskbar/render.rs:222-237`, `:320-341`; `src/ui/mod.rs:569-574`, `:707-729`; both windows do their own DPI scaling and `WM_DPICHANGED` relayout). What is missing: there is no dark/light **mode** — you edit `#RRGGBB` strings in JSON by hand, and `Config::save` has no caller, so the app writes the file exactly never. `Theme.opacity` is read only as `== 0` (`render.rs:222`) so partial transparency is unreachable, and `Retention` (`src/config/mod.rs:52-58`) is parsed and discarded.

- Next step: add a `theme.mode: "dark" | "light" | "system"` field with two built-in palettes that overwrite the four colour strings on load, plus a real `opacity: u8` → alpha path (or document that only 0/255 are honoured).
- Dashboard page: a new `Settings` page (page index 5) — themes are a preference, not a metric.

### Dashboard — a page list, not yet a dashboard
The window is real and finished as a shell: lazy creation on first click, hide-on-close (`src/ui/mod.rs:439-442`), a `LISTBOX` sidebar themed through `WM_CTLCOLORLISTBOX` (`:370-379`), sidebar shade derived from the theme's own luminance (`:299-312`), DPI-scaled layout, minimum-size floor, and a test asserting every page in `PAGES` is reachable (`:765-774`). What is missing vs "ringkasan menyeluruh … beserta statistik utama": the four pages render the same eight taskbar strings, plus a sparkline on Overview and Data only (`:244-286`). There are no totals, no peaks, no per-day figures, no adapter table.

- Next step: the pages have no data to show — build the Network Info and Usage Stats collectors below, then add rows to `page_rows` for the matching page. `page_rows` is the single extension point.
- Dashboard page: Overview (statistics land here), with detail on the other three.

### Data Plan — works, but you cannot set the plan in the app
Quota percentage and the over-plan colour are correct and deliberately unclamped so overage is visible (`src/telemetry/usage.rs:145-150`). The gaps: `quota_gb` is only settable by editing `%APPDATA%\ArboTray\config.json` before launch (default `0.0` hides the feature entirely, `src/config/mod.rs:19`, `:69`), and `quota_pct` is used solely as a boolean (`src/app.rs:81-84`) — the user never sees "42% of plan" anywhere. Nothing in IMPROVE.md's "mengatur … kuota" is met.

- Next step: add a "Quota" row to the Data page that prints the percentage, and expose `quota_gb` for editing once a settings UI exists (`Config::save` is already written and waiting).
- Dashboard page: Data.

### WiFi — read-only, one snapshot
Band, SSID and signal all work (see Done above). Missing vs "kekuatan sinyal, hingga pengelolaan kata sandi yang tersimpan": no signal history or quality trend, no `WlanGetProfile`/`WlanSetProfile` for saved-network password management, and no `WlanGetAvailableNetworkList` scan — the collector only ever queries the interface that is already connected (`src/telemetry/wifi.rs:115-132`). On a desktop with no WLAN card `poll()` returns `None` and the feature simply disappears, which is correct behaviour but means there is nothing to fall back to.

- Next step: add `WlanGetAvailableNetworkList` for a scan list on the Network page; password management is gated on a settings UI and on writing credentials, which is a larger security decision.
- Dashboard page: Network.

### Usage Stats — today only, retention is dead code
The daily accumulator is exact (deltas from cumulative counters) and survives counter resets and date changes (`src/telemetry/usage.rs:78-89`, `:110-123`). Missing vs "pembagian penggunaan kuota harian dan bulanan": there is no history at all — one file, one day, two integers, and yesterday is overwritten at midnight (`:111-123`). The `Retention { raw_days, minute_days, hour_days }` config (`src/config/mod.rs:52-58`) is declared, serialised and never read by any code (`grep -rn retention src/` matches only that block and a doc comment), so the planned raw→minute→hour aggregation described in `docs/PLAN.md:29-32` does not exist. There is also no `src/storage/` module.

- Next step: append one record per day to a `history.json` array instead of overwriting, and add a per-day bar list to the Data page. `Retention` should either be honoured here or deleted.
- Dashboard page: Data.

### Network Info — gateway latency and SSID only
What reaches a user: gateway round-trip (`src/taskbar/mod.rs:57-60`, Network page row at `src/ui/mod.rs:259`) and the SSID (`src/ui/mod.rs:253-255`). Missing vs "alamat IP, DNS, gateway, dan detail koneksi": no local IP, no gateway *address* (the gateway `u32` is computed and thrown away — `src/telemetry/latency.rs:171-178` pings it but never formats it), no DNS servers, no adapter description. Two collected values are dropped on the floor: `LatencySample.loss_pct` is computed from a 10-probe window (`latency.rs:35-41`, `:196`) and read by no UI code, and `internet_ms` is hardcoded `None` with the target constant parked behind `#[allow(dead_code)]` (`:182-183`, `:201-206`).

- Next step: cheapest win in the list — add a Network page row for packet loss (the number already exists) and wire the 8.8.8.8 probe, then add local IP/DNS via `GetAdaptersAddresses`.
- Dashboard page: Network.

## Not done

### Always-on-Top Widget
The IMPROVE.md item is a floating, draggable, always-topmost widget. ArboTray's design is the opposite: it is a `WS_CHILD` of `Shell_TrayWnd` (`src/taskbar/dock.rs:126-147`) with `WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`, deliberately docked and unmovable. No `HWND_TOPMOST` / `WS_EX_TOPMOST` appears anywhere, and the dashboard is a normal window using `WS_OVERLAPPEDWINDOW` (`src/ui/mod.rs:172`).

- Next step: treat as a deliberate divergence rather than a gap, or add a docked/strip position choice. A true floating widget means a second top-level window with `WS_EX_TOPMOST`, owning its own paint loop — a new module, not a flag.
- Dashboard page: none — window-level trait, no page.

### Settings (UI)
Nothing. `Config::save()` is fully implemented — pretty-printed JSON, directory creation (`src/config/mod.rs:138-145`) — and called from nowhere in `src/`. There is no way in the app to change interval, visible tiles, colours, quota or font. Editing requires closing the app, hand-writing JSON, and relaunching; app.rs reads the config once at startup (`src/app.rs:30`).

- Next step: highest value per line of code. Add a `Settings` page that reads `Config` from `WindowState.cfg`, writes edits back with the existing `save()`, and calls `Renderer::set_metrics` (`src/taskbar/render.rs:136-145`) — which already exists for exactly this "settings edit" case, per its own comment, and also has no caller.
- Dashboard page: new `Settings` page appended to `PAGES`.

### Adapter Config
No adapter enumeration or selection. Throughput is the sum of every up non-loopback interface (`src/telemetry/network.rs:44-50`), and the Wi-Fi collector only reports whichever interface the WLAN API says is connected (`src/telemetry/wifi.rs:125-128`). The Windows bindings do include `Win32_NetworkManagement_IpHelper` (`Cargo.toml:15`), so `GetAdaptersAddresses` is available without a dependency change.

- Next step: enumerate adapters with `GetAdaptersAddresses`, list them on the Network page with per-adapter totals, and add a `config.adapter` selector that filters `read_counters`.
- Dashboard page: Network.

### Network Tools
Only a fixed gateway ICMP probe exists (`src/telemetry/latency.rs:93-121`). No traceroute, no DNS lookup/check, no connection analysis, no user-entered target — the probe destination is derived internally from `GetBestRoute2` and never exposed.

- Next step: a target input plus `IcmpSendEcho` with a rising TTL gives traceroute; `DnsQuery` or `getaddrinfo` gives the DNS check. Both need an input control, which needs the settings-page infrastructure first.
- Dashboard page: Network.

### Speed Test
Nothing. No on-demand measurement, no upload path, no `download_test`. The existing collector measures passive throughput only — it can never generate load (`src/telemetry/network.rs` has no write path; `tx_bps` is read from `row.OutOctets`).

- Next step: largest new subsystem here. Needs an HTTP client (a new dependency — nothing in `Cargo.toml` can fetch), a chosen test endpoint, a progress UI, and a result model. Decide the endpoint and the dependency question first.
- Dashboard page: Network.

### Test History
Nothing to store — see Speed Test. No result struct, no persistence beyond the single-day `usage.json` (`src/telemetry/usage.rs:26-33`).

- Next step: blocked on Speed Test; then append results to a JSON list under `%APPDATA%\ArboTray\` and render as a table with timestamps.
- Dashboard page: Network.

### Network Interface
Per-interface statistics are discarded at the source: `GetIfTable2` rows are iterated and summed into one `(rx, tx)` pair, with only `OperStatus` and loopback used for filtering (`src/telemetry/network.rs:41-53`). No interface name, speed, MTU or error counters survive the loop.

- Next step: return a `Vec<InterfaceRow>` instead of a summed pair (or add a second function), and render a table. Note `app.rs:74` and `usage.rs` depend on the summed `totals()` contract, so keep that shape and add a parallel detail read.
- Dashboard page: Network (or System, if framed as hardware).

### Active Process
Nothing. No `GetProcessIoCounters`, no `EnumProcesses`, no ETW session, no `GetExtendedTcpTable` PID mapping. `docs/PLAN.md:25` reserves a `connections.rs` module for it; that file does not exist. The README states plainly that Windows exposes no per-process byte counter without ETW.

- Next step: the hardest item. `GetExtendedTcpTable` + `GetProcessIoCounters` gives per-PID totals (not per-connection rates); true real-time per-app bandwidth needs an ETW kernel session. Scope the acceptable approximation before starting.
- Dashboard page: Network.

### Stopwatch
Nothing. No timer, no session start/stop, no elapsed-time state. The only timing state anywhere is network-delta interpolation (`src/telemetry/network.rs:62`) and the 30 s save throttle (`src/telemetry/usage.rs:21`).

- Next step: trivially self-contained — a start/stop `Instant` plus a `WM_TIMER` (or tick off the existing 1 Hz sample) rendering `HH:MM:SS` on a page. Needs a button, i.e. some UI plumbing the sidebar `LISTBOX` does not currently offer.
- Dashboard page: Overview (or its own page).

### Port Active
Nothing. No TCP/UDP table call of any kind: `grep -rE 'GetExtendedTcpTable|GetTcpTable|GetUdpTable' src/` returns zero matches. No listening-port model, no protocol/process display.

- Next step: `GetExtendedTcpTable` with `TCP_TABLE_OWNER_PID_ALL`, then resolve each PID to a name via `OpenProcess` + `QueryFullProcessImageNameW`, and render a scrollable table. Needs `Win32_Networking_WinSock` (already enabled, `Cargo.toml:14`) plus likely `Win32_System_ProcessStatus` (also already enabled).
- Dashboard page: Network.

## Cross-cutting finding (not an IMPROVE.md item)

`src/taskbar/events.rs:71-76` handles Explorer restart by posting `WM_QUIT`, with the comment "quit and let the supervisor in `app` re-attach to the new one." **No supervisor exists** — `app::run` calls `message_loop()` once and returns (`src/app.rs:51-57`), so the process simply exits. `README.md` documents this correctly under Known limits; the code comment does not. Worth fixing the comment or adding the supervisor loop.

## Suggested next

Ordered by effort-to-value:

1. **Settings page** — `Config::save()`, `Renderer::set_metrics()` and `UiState.cfg` all already exist and are unused; this one page unlocks theme, quota, interval, font and tile visibility that are currently hand-edit-only. Lowest effort, highest value.
2. **Surface data already collected** — `LatencySample.loss_pct` is computed and dropped, `internet_ms` is parked behind a dead-code allow, and the gateway address is pinged but never printed. Adding Network-page rows is a display change plus wiring the 8.8.8.8 probe; near-zero risk, immediately visible.
3. **Network Info page** — `GetAdaptersAddresses` (bindings already enabled) for local IP, gateway address, DNS and adapter name. Fills the emptiest page with the data half of Premium.
4. **Daily/monthly Usage Stats** — append per-day records instead of overwriting `usage.json`, honour or delete the dead `Retention` config, and draw the breakdown on the Data page. The accumulator is already exact.
5. **Speed Test** — highest value of the remaining Premium items but the only one needing a new dependency and a chosen endpoint; decide those two things before writing code.
