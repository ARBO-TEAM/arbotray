//! Orchestrator: wires config, telemetry and the taskbar window together.
//!
//! Threading: the taskbar window owns its thread because Win32 demands that a
//! window's message loop live where it was created. Telemetry polls on a
//! background thread and hands `TrayModel`s across a channel, waking the UI
//! with a `PostMessageW` so nothing has to poll on a timer.

use crate::config::{Config, interval_duration};
use crate::taskbar::dock::AttachResult;
use crate::taskbar::{Notifier, Tray, TrayModel};
use crate::telemetry::{Sampler, SpeedTest};
use std::time::Duration;
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::w;

/// How many samples the mini-sparkline keeps. 60 at 1 Hz = one minute.
pub const HISTORY_LEN: usize = 60;

pub fn run() -> i32 {
    let _guard = match SingleInstance::acquire() {
        Single::First(guard) => guard,
        Single::AlreadyRunning => return 0,
    };

    // DPI awareness must be set before any window exists, or the tray child
    // window renders at the wrong scale on high-DPI displays.
    set_dpi_awareness();

    let cfg = Config::load();

    // The speed test is owned here rather than by the telemetry thread: it is
    // started by a button on a window the telemetry thread has no handle to,
    // and its runs outlive the sample that started them. It is handed to both
    // the sampler — which folds the current run into each model — and the
    // dashboard, which starts runs.
    let speed = SpeedTest::new();

    let (tray, notifier) = match attach_with_retry(&cfg, speed.clone()) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("arbotray: cannot attach to taskbar: {e}");
            return 1;
        }
    };

    let telemetry = std::thread::Builder::new()
        .name("telemetry".into())
        .spawn(move || telemetry_loop(notifier, speed));

    if let Err(e) = telemetry {
        eprintln!("arbotray: cannot start telemetry thread: {e}");
        return 1;
    }

    let mut tray = tray;
    if let Err(e) = tray.message_loop() {
        eprintln!("arbotray: message loop failed: {e}");
        return 1;
    }
    0
}

/// How long to keep looking for the taskbar, and how often to look again.
///
/// The first attempt is immediate, so a normal launch is unchanged. The wait is
/// for one launch path only: started by Windows at logon, this process can be
/// running before Explorer has created `Shell_TrayWnd`. A single attempt would
/// fail there, return 1, and — under `windows_subsystem = "windows"` — do it
/// with nothing on screen, so Start with Windows would look like it did nothing
/// at all. A scheduled task could instead delay the launch, which is what the
/// reference implementation does; waiting here is the same fix without needing
/// one, and it also covers Explorer being restarted later.
const ATTACH_TIMEOUT: Duration = Duration::from_secs(20);
const ATTACH_RETRY: Duration = Duration::from_millis(250);

/// `Tray::attach`, retried until the taskbar exists or the deadline passes.
///
/// The last error is what comes back, so a genuine failure still names itself
/// rather than reporting the first sighting.
fn attach_with_retry(cfg: &Config, speed: SpeedTest) -> AttachResult<(Tray, Notifier)> {
    let deadline = std::time::Instant::now() + ATTACH_TIMEOUT;
    loop {
        match Tray::attach(cfg, speed.clone()) {
            Ok(pair) => return Ok(pair),
            Err(e) if std::time::Instant::now() >= deadline => return Err(e),
            Err(_) => std::thread::sleep(ATTACH_RETRY),
        }
    }
}

/// Polls forever, pushing a freshly formatted model on each tick.
/// The `Sampler` is built *inside* the thread: several collectors own raw
/// Win32 handles and are not `Send`.
///
/// The config is re-read from the notifier every tick rather than captured at
/// startup. This is the half of a Settings-page edit that the window cannot do
/// for itself: tile visibility and the refresh period are only ever consulted
/// here, so a captured copy would leave the file changed and the taskbar
/// unchanged — the failure the Settings page exists to remove.
fn telemetry_loop(notifier: crate::taskbar::Notifier, speed: SpeedTest) {
    let mut sampler = Sampler::new();
    let mut history: Vec<u64> = Vec::with_capacity(HISTORY_LEN);
    // Loaded once and carried across ticks: it accumulates deltas, so a fresh
    // one per poll would have no baseline and count nothing. The retention
    // window comes from the config as it was at startup, like every other
    // setting that only shapes what is on disk.
    let mut usage = crate::telemetry::Usage::load(notifier.config().retention.days);

    loop {
        let cfg = notifier.config();
        let metric = sampler.poll();

        // Feed the usage counter from the raw cumulative octets, not from
        // `metric.net` — that is already divided into a rate.
        if let Some((rx, tx)) = sampler.net.totals() {
            usage.record(rx, tx);
        }
        usage.flush_if_due();

        let mut model = TrayModel::from_metric(&metric, &cfg);
        if cfg.show.usage {
            model.usage_text = usage.text();
            model.quota_alert = usage
                .quota_pct(cfg.quota_gb)
                .is_some_and(|pct| pct >= 100.0);
        }
        // Not gated on `show.usage`: like the gateway and adapter rows these
        // are Data-page detail with no tile of their own, absent from
        // `render::visible_segments`, so they cannot widen the strip.
        model.month_text = crate::telemetry::usage::format_size(usage.month_bytes());
        model.month_partial = !usage.covers_whole_month();
        model.usage_days = usage.history();
        // The speed test's own state, folded in here like the usage counter
        // above and for the same reason: a run outlives the sample it was
        // started in, so it cannot be rebuilt from a metric. This is also what
        // drives the page while a test runs — the worker thread has no window
        // to invalidate, so the next tick is what shows its progress.
        model.set_speed(&speed.snapshot());

        if let Some(net) = metric.net {
            if history.len() == HISTORY_LEN {
                history.remove(0);
            }
            history.push(net.rx_bps);
        }
        model.history = if cfg.show.sparkline {
            history.clone()
        } else {
            Vec::new()
        };

        notifier.send(model);
        // The sleep is rebuilt from the config just read, so an interval edit
        // takes effect on the very next wait rather than at the next launch.
        std::thread::sleep(interval_duration(&cfg));
    }
}

/// Per-monitor-v2 DPI awareness. Failure is survivable — the app just renders
/// slightly soft on scaled displays — so this is deliberately best-effort.
fn set_dpi_awareness() {
    use windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

// --- single instance ------------------------------------------------------

enum Single {
    First(SingleInstance),
    AlreadyRunning,
}

/// A named mutex. Dropping the last handle is what releases the claim, so this
/// deliberately holds the `HANDLE` rather than relying on process exit.
struct SingleInstance(HANDLE);

impl SingleInstance {
    fn acquire() -> Single {
        unsafe {
            match CreateMutexW(None, true, w!("ArboTray.SingleInstance")) {
                Ok(handle) => {
                    if GetLastError() == ERROR_ALREADY_EXISTS {
                        // We still own a handle to the existing mutex; drop it
                        // so we don't keep the other instance alive.
                        let _ = CloseHandle(handle);
                        Single::AlreadyRunning
                    } else {
                        Single::First(SingleInstance(handle))
                    }
                }
                Err(_) => Single::AlreadyRunning,
            }
        }
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
