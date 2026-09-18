# ArboTray Feature Status

Audit of the feature list in `docs/IMPROVE.md` against the source tree of **arbotray v0.10.0** (`Cargo.toml:3`) — refreshed 2026-09-18. The previous revision covered v0.9.0.

All features are available to every user — there is no free tier, no premium tier, and no licence gate.

Every row below was verified against working code. A config field, a struct field or a TODO comment is not counted as a feature: **Done** means a user can see or use it today.

Tree today: 18,653 lines of Rust across `src/`, **243 tests passing, 2 ignored**.

## Summary

| Feature | Status | Evidence |
| --- | --- | --- |
| Live Speed Widget | Done | `src/taskbar/mod.rs:48-55` formats into `down_text`/`up_text`; painted at `src/taskbar/render.rs:240-253` |
| Always-on-Top Widget | Done | `src/widget/window.rs:231` `HWND_TOPMOST`; draggable by `HTCAPTION` (`:30`), 4 modules under `src/widget/` |
| System Tray Icon | Partial | Icon, tooltip and menu complete (`src/taskbar/icon.rs:72-163`), but still no `NIF_INFO` — zero matches tree-wide |
| Settings (UI) | Done | Page 8 of the sidebar, two cards: "Display Tiles" (the 8 tile checkboxes, 2×4) and "Preferences" (11 rows, ending in the Save/Reload pair) |
| Adapter Config | Not done | `GetAdaptersAddresses` reads the routed interface (`src/telemetry/adapter.rs:95`) but only to display it; no `config.adapter` field exists |
| Dark & Light Theme | Done | Appearance row swaps both colour fields between presets (`src/ui/design.rs` `next_preset`); staged behind Save |
| Dashboard | Done | 9-page sidebar (`src/ui/pages.rs:12`), charts with axes (`src/ui/chart.rs`), reusable modal, DPI-scaled |
| Data Plan | Partial | `quota_gb` is now editable on the Settings page (`ROW_PLAN`, `src/ui/settings.rs:542`); the percentage is still never printed |
| WiFi | Partial | SSID, band, signal read (`src/telemetry/wifi.rs:101-111`); no scan, no profile management, no history |
| Network Tools | Not done | Zero matches for `DnsQuery`/`getaddrinfo`/TTL — the only ICMP is the fixed gateway probe |
| Speed Test | Done | `src/telemetry/speedtest.rs` — on-demand throughput over WinHttp, no new crate |
| Test History | Done | `HISTORY_MAX = 10` in memory (`src/telemetry/speedtest.rs:70`, `:238-239`) |
| Usage Stats | Done | Rolling daily records in `usage.json`, capped by `Retention.days`; Data page shows today, month and last seven |
| Network Info | Done | Gateway, latency, packet loss, adapter, local IP, resolver list (`src/telemetry/adapter.rs`, `latency.rs`) |
| Network Interface | Not done | `GetIfTable2` rows are still summed into one pair and discarded (`src/telemetry/network.rs:71-103`) |
| Active Process | Not done | PID→name mapping exists (`src/telemetry/ports.rs:365`) but there is no per-process byte counter |
| Stopwatch | Done | `src/ui/stopwatch.rs` — start/stop/reset, `HH:MM:SS` on its own page |
| Port Active | Done | `src/telemetry/ports.rs` — `GetExtendedTcpTable` with owner PID, scrollable list, Stop behind a confirmation |
| Timer | Done | `src/power.rs` engine + page 7; sleeps or shuts down on a clock time or a countdown, behind a countdown popup |
| Update check | Done | `src/update.rs` — background check, version on the System page's "This machine" card, banner when a newer release exists |
| Start with Windows | Done | `ROW_STARTUP` toggle writing the `Run` key |
| Tray Notifications | Done | `Notify` config, `alert::Alerts` engine (quota threshold + rate rising-edge), `Icon::balloon` via `NIF_INFO`, Settings rows in Preferences card |

Counts: **15 Done, 3 Partial, 4 Not done** — 22 rows, counted from the table above, which is the authority.

The nine pages are `Overview` (0), `Network` (1), `System` (2), `Data` (3), `Ports` (4), `Speed Test` (5), `Stopwatch` (6), `Timer` (7), `Settings` (8). The `OVERVIEW`/`NETWORK`/…/`SETTINGS` constants are **positional**, so a new page must be *appended* to `PAGES` — inserting one renumbers every page after it, and the labels would still read correctly while the routing broke.

## Done

### Live Speed Widget
The product's core and unchanged since v0.2.0. `Sampler::poll` reads cumulative octet counters over every up, non-loopback, hardware interface and divides the delta by elapsed time (`src/telemetry/network.rs`); `TrayModel::from_metric` formats both directions (`src/taskbar/mod.rs:48-55`); the renderer draws them into the taskbar next to the clock, with a 60-sample download sparkline. Counter resets are handled as lost deltas rather than spikes.

### Always-on-Top Widget
The IMPROVE.md item the v0.2.0 audit recorded as a deliberate divergence, now built as its own top-level window rather than a flag on the taskbar strip. `src/widget/` holds four modules — `window.rs` (creation, drag, topmost, its own paint loop), `render.rs`, `rows.rs`, `metrics.rs` — with tests in three of them. It is `HWND_TOPMOST` (`window.rs:231`), moves by `WM_NCHITTEST` returning `HTCAPTION` (`:30`), and shows traffic plus CPU and RAM rather than the full taskbar string, because at panel size the rest was unreadable.

### System Tray Icon — still no notifications
Icon, tooltip and the right-click menu are complete, including the drop-time `NIM_DELETE` that prevents ghost icons and the truncating copy that cannot overrun `szTip` (`src/taskbar/icon.rs:86-163`). The missing half is unchanged: no `NIF_INFO`, no balloon, no toast. The icon reflects live speed in its hover text but never pushes an alert.

- Next step: on the existing `WM_TRAY_UPDATE` path, when `quota_alert` flips false→true, call `Shell_NotifyIconW(NIM_MODIFY)` with `uFlags |= NIF_INFO`. Reuse `WindowState.model` to detect the edge; do not add a timer.
- Dashboard page: none — tray-level.

### Settings page
Page 8, and now two rounded cards rather than a flat list of rows. Card 1, "Display Tiles", holds the eight tile checkboxes in two columns by four rows; card 2, "Preferences", holds eleven rows — Refresh, Monthly plan, Font size, Background, Foreground, Alert, Opacity, Start with Windows, Desktop widget, Appearance, and the Save/Reload pair under the caption `Write config.json`. The divider that used to separate the two halves is gone; the card edge does that job now.

`layout::START_H` is pinned to the **System** page's five-card stack, not to this one — System is the taller of the two. All three colour rows pair a hex field with a `ChooseColorW` picker (`src/ui/settings.rs:312`); a dismissed dialog writes nothing.

The controls are native children — `EDIT` for the fields, `BS_OWNERDRAW` buttons for everything with a face — and the page paints only their captions and the cards behind them. `WM_DRAWITEM` sets `TRANSPARENT` on its DC before drawing any glyph: a `DrawTextW` left opaque fills its own text extent with the brush it was handed, which erased the accent square under a checked box's tick and cut a card-coloured hole in Save's fill.

The page never holds a `Config` while the user types — `SettingsForm` keeps raw strings and `into_config` is the single place a typed value becomes a setting, so an emptied numeric field is "no value yet" rather than a `0` that erases the setting. Save writes through `Config::save()`; a write that fails reports it rather than claiming success. Nothing on the page is live until Save runs, and the page says so.

### Dark & Light Theme
The v0.2.0 audit's "no theme picker, you edit `#RRGGBB` by hand" is closed. The Appearance row (`ROW_APPEARANCE`) carries a glyph and a caption that name the mode the button switches **to**, and one click types the other preset's two colours into the Background and Foreground fields above it.

It is deliberately **not** a `theme.mode` field: a preset is a pair of hex strings and nothing else, because that is all the theme already is. A stored mode beside two colour strings would be a second place the appearance is written down and a first place it can be wrong. Staged behind Save like everything around it, so it is safe to try and safe to undo. `next_preset` reads the luma the whole palette is already built on, so "the window looks light" and "the button offers dark" cannot disagree.

### Dashboard
Nine pages behind a sidebar, and the shell is no longer the interesting part — the pages have data. `src/ui/chart.rs` draws the sparklines and the labelled charts, `src/ui/modal.rs` is the one reusable confirmation popup (owner-drawn, because a `MessageBoxW` cannot be told that one of its two buttons is destructive), and the whole window is DPI-scaled with `WM_DPICHANGED` relayout. `page_rows` remains the single extension point.

### Usage history — daily and monthly
`usage.json` keeps a rolling window of daily byte records, oldest first, capped by `Retention.days` (default 7). Both counts come from deltas of the cumulative interface counters, so the totals are exact rather than a sum of rounded rates; a counter that goes backwards is an adapter reset and its delta is dropped rather than underflowing. The Data page shows today, the month total and the last seven days, and the month row's caption becomes `Month so far` when the window no longer reaches the first of the month — a partial sum must not read as month-to-date.

Today's bytes are accumulated from raw counters, persisted to `%APPDATA%\ArboTray\usage.json` at most every 30 s, and rolled over at local midnight. Over-quota recolours the whole taskbar run and the dashboard.

### Network Info
`IcmpSendEcho` against the default route and a public resolver, probed every 3 s and cached between polls so the tray never stalls (`src/telemetry/latency.rs`). The gateway renders as `NNms` with a `--` placeholder when unreachable; the Network page adds the gateway address, internet latency, packet loss, the routed adapter's name and local address, and every resolver Windows was handed.

Addresses arrive from Win32 in **network byte order** — the bytes are the address and the numeric value is not, so `Ipv4Addr::from(u32)` silently prints `192.168.1.1` as `1.1.168.192`. All of them go through `crate::taskbar::format_addr`, pinned by `addresses_are_read_in_network_byte_order`.

### Port Active
`GetExtendedTcpTable` with `TCP_TABLE_OWNER_PID_ALL`, each PID resolved to a name via `OpenProcess` + `QueryFullProcessImageNameW` (`src/telemetry/ports.rs:365`). The whole list is rendered, scrollable, rather than the first twelve that fit — a dev machine holds more listeners than fit between the counters and the Stop button. Stopping a process goes through the shared modal: the row is selected first, then the Stop button raises the question, and the confirm button wears the danger colour because killing a process is not recoverable.

### Stopwatch
Its own page and its own window timer (`TIMER_WATCH`). Start/Stop/Reset, the reading in the clock face at body height rather than the small text a row would give it, and a caption read back off the clock so a config edited by hand and a page showing the wrong word cannot both be true.

### Timer
The sleep / shut-down timer, and the newest feature in the tree. Four settings — mode (a clock time or a countdown), the time, the minutes, and whether to sleep or shut down — plus an Arm button that is deliberately **not** a setting: everything above it is stored in `config.json` and survives a restart, and the arm is a decision about tonight that does not. It writes to disk the moment it is clicked rather than behind Save.

`src/power.rs` owns every decision as a pure function: `fire_at`, `phase` (waiting / warning / due / stale), `format_countdown`, `action_of`. The watch is a second window timer armed only while a timer is, so a machine with nothing set pays nothing.

Nothing fires without being asked. A minute before the instant the popup comes up carrying a live countdown; Cancel disarms rather than merely answering, because an answered question that stayed armed would be back a second later and the only way to keep a machine up would be to keep cancelling. `Esc` and a page change take the question down the same way, through one method, because all three mean the same thing. The disarmed-before-firing order in `run_action` matters: a suspend that succeeds never returns, so a clear written afterwards would only ever run on the failure path.

A timer whose moment passed while the machine was asleep is dropped with a note rather than fired at — that is what the engine's five-minute grace window is for.

### Update check
`src/update.rs`: a background check that never blocks a paint, the version printed as a `Version` row on the System page's "This machine" card (`update::current()`, i.e. `CARGO_PKG_VERSION`, now `0.10.0`), and a banner naming the newer release and where to get it. No auto-install — the app tells, the user decides.

## Partial

### System Tray Icon — no notifications
See Done above for what ships. This is the only remaining gap against the original IMPROVE.md wording, and it is worth one small change rather than a subsystem.

### Data Plan — the percentage is never shown
`quota_gb` became editable when the Settings page gained its field, which closes half of what the v0.2.0 audit recorded. The other half stands: `quota_pct` is computed (`src/telemetry/usage.rs:251`) and used solely as a boolean — over plan or not — so the user sees a recoloured window but never "42% of plan" anywhere. `clamp_quota_gb` (`src/config/mod.rs:285`) will reject a nonsense entry.

- Next step: add a "Plan" row to the Data page printing the percentage. `quota_pct` already exists and the collector already runs.
- Dashboard page: Data.

### WiFi — read-only, one snapshot
Band, SSID and signal all work. Missing against "kekuatan sinyal, hingga pengelolaan kata sandi yang tersimpan": no signal history or trend, no `WlanGetProfile`/`WlanSetProfile`, and no `WlanGetAvailableNetworkList` scan — zero matches tree-wide, since the collector only ever queries the interface already connected. On a desktop with no WLAN card `poll()` returns `None` and the feature disappears, which is correct behaviour but means there is nothing to fall back to.

- Next step: `WlanGetAvailableNetworkList` for a scan list on the Network page. Password management is gated on writing credentials, which is a security decision before it is a coding one.
- Dashboard page: Network.

### Theme opacity — a real value on the widget, a switch on the strip
`theme.opacity` means two different things, and both are now honest. On the **taskbar strip** it is still read only as `== 0` (`src/taskbar/render.rs:222`): zero means "sample the taskbar colour underneath and paint with it", anything else means "use the configured background" — the strip is a child of `Shell_TrayWnd`, which refuses `WS_EX_LAYERED`, so a real alpha is not available to it. On the **desktop widget** it is a genuine alpha passed to `SetLayeredWindowAttributes` (`src/widget/window.rs:293-299`), with `0` reading as `DEFAULT_ALPHA` = 190 rather than as fully opaque. So the `Opacity   0-255` row is a real number for the panel and a yes/no for the strip, which is one setting meaning one thing per surface rather than a lie.

- Next step: the strip could take a real alpha the way the panel does if it ever moves off `Shell_TrayWnd`; until then the row's caption is the place to say which surface it moves.
- Dashboard page: none — tray-level.

## Not done

### Adapter Config
`GetAdaptersAddresses` is available and used (`src/telemetry/adapter.rs:95`, `:200`) but only to *report* the routed interface on the Network page. Throughput is still the sum of every up, non-loopback, hardware interface (`src/telemetry/network.rs:44-50`, `:71-103`), and there is no `config.adapter` field to select one.

- Next step: list adapters on the Network page with per-adapter totals, and add a `config.adapter` selector that filters `read_counters`. `GetAdaptersAddresses` is already enabled in `Cargo.toml` with no dependency change.
- Dashboard page: Network.

### Network Interface
Per-interface statistics are discarded at the source: `GetIfTable2` rows are iterated and summed into one `(rx, tx)` pair, with only `OperStatus`, loopback and the hardware bit used for filtering. No interface name, speed, MTU or error counter survives the loop.

- Next step: return a `Vec<InterfaceRow>` instead of a summed pair (or add a second function beside it). Note `app.rs` and `usage.rs` depend on the summed contract, so keep that shape and add a parallel detail read.
- Dashboard page: Network, or System if framed as hardware.

### Network Tools
Only a fixed gateway ICMP probe exists (`src/telemetry/latency.rs`). No traceroute, no DNS lookup or check, no connection analysis, no user-entered target — the probe destination is derived internally from `GetBestRoute2` and never exposed.

- Next step: a target input plus `IcmpSendEcho` with a rising TTL gives traceroute; `getaddrinfo` gives the DNS check. Both need a text-entry control on a page, which the Settings page's field plumbing can now supply.
- Dashboard page: Network.

### Active Process
The hardest item, and still the hardest. Per-PID *identification* now exists inside the ports collector — `GetExtendedTcpTable` gives the owner PID and `QueryFullProcessImageNameW` gives its name — but that is which process owns a socket, not how much traffic it moved. There is no `GetProcessIoCounters`, no ETW session, and no per-process byte counter. README states plainly that Windows exposes no cheap per-process byte counter without ETW.

- Next step: `GetProcessIoCounters` + the existing PID mapping gives per-PID totals (not per-connection rates, and not network-specific — it counts disk too). True real-time per-app bandwidth needs an ETW kernel session. Scope which approximation is acceptable before starting.
- Dashboard page: Network.

## Cross-cutting finding (not an IMPROVE.md item)

`src/taskbar/events.rs` handles an Explorer restart by posting `WM_QUIT`, and the comment there is honest about it — there is still **no supervisor**. `app::run` calls `message_loop()` once and returns, so the process simply exits. The real fix is a re-attach loop in `app`, and it is not written. Unchanged from the v0.2.0 audit; not made worse by anything since.

## Suggested next

Ordered by effort-to-value:

1. **Explorer-restart supervisor** — the one outright correctness bug left. Re-attach instead of exiting; the comment already says so.
2. **Notifier balloons** — `NIF_INFO` on the tray icon for quota alerts. The icon is installed and its tooltip already updates; this is the smallest remaining item.
3. **Data Plan percentage** — one row on the Data page over a `quota_pct` that already exists.
4. **Per-interface stats** — return a `Vec<InterfaceRow>` alongside the summed contract that `app` and `usage` depend on; unlocks Adapter Config after it.
5. **Opacity caption** — the value is real on the panel and a switch on the strip; the row could say which.
