//! Docking a child window into the taskbar.
//!
//! The window is created as a child of `Shell_TrayWnd`, sized to the taskbar's
//! own height and parked immediately left of `TrayNotifyWnd` (the clock and
//! notification area), which is the free real estate tray monitors use
//! use. Being a real child window means the taskbar clips and moves it for us.

use crate::config::Config;
use crate::taskbar::TrayModel;
use crate::taskbar::events::{WM_TRAY_UPDATE, WindowState, wnd_proc};
use crate::taskbar::icon::Icon;
use crate::taskbar::render::Renderer;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, DispatchMessageW, FindWindowExW, FindWindowW, GetMessageW,
    GetWindowRect, MSG, RegisterClassW, RegisterWindowMessageW, TranslateMessage, WM_APP,
    WNDCLASSW, WS_CHILD, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_VISIBLE,
};
use windows::core::w;

/// Why `attach` failed. A string is enough — there is nothing a caller can do
/// with the failure but report it, and the underlying Win32 text is the useful
/// part.
pub type AttachResult<T> = std::result::Result<T, String>;

/// Fallback when the DPI cannot be read.
const DEFAULT_DPI: u32 = 96;

/// How many sparkline samples a reserved width accounts for.
const RESERVED_HISTORY: usize = 60;

/// The live tray window. Owns the window state its `WndProc` reads.
pub struct Tray {
    hwnd: HWND,
    /// Kept boxed so its address is stable for `GWLP_USERDATA`.
    #[allow(dead_code)]
    state: Box<WindowState>,
}

/// Hand a new config to the telemetry thread, which formats every sample
/// through it.
///
/// A poisoned mutex is recovered from rather than propagated: the telemetry
/// thread cannot panic with the lock held (its body is arithmetic and a channel
/// send), and a poisoned read is still a readable `Config`. Refusing here would
/// leave the tray painting a setting the file no longer holds.
pub fn share_config(shared: &Arc<Mutex<Config>>, cfg: &Config) {
    match shared.lock() {
        Ok(mut guard) => *guard = cfg.clone(),
        Err(poisoned) => *poisoned.into_inner() = cfg.clone(),
    }
}

/// Handed to the telemetry thread. Safe to call from any thread: it only sends
/// on a channel and posts a message, both of which are thread-safe.
pub struct Notifier {
    sender: Sender<TrayModel>,
    hwnd: HWND,
    /// The live config, shared with the window. The telemetry thread reads it
    /// once per tick rather than owning a copy, so a Settings-page edit reaches
    /// the tile it changes within one poll — and the refresh period change
    /// takes effect on the sleep that follows the same tick.
    config: Arc<Mutex<Config>>,
}

// SAFETY: `PostMessageW` is documented as callable against a window owned by
// another thread, and `Sender` is `Send`. `HWND` is only ever passed to that
// one API.
unsafe impl Send for Notifier {}

impl Notifier {
    /// The current config, for the telemetry thread to format with.
    ///
    /// A clone per tick rather than a borrow held across the sleep: the UI
    /// thread takes the lock on a settings edit, and holding it between ticks
    /// would stall that write for up to the refresh period.
    pub fn config(&self) -> Config {
        match self.config.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Queue a sample and wake the UI thread. Failures are ignored: a closed
    /// channel or a destroyed window just means shutdown is under way.
    pub fn send(&self, model: TrayModel) {
        if self.sender.send(model).is_err() {
            return;
        }
        // SAFETY: posting to a window we do not own is explicitly allowed.
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                Some(self.hwnd),
                WM_TRAY_UPDATE,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
}

impl Tray {
    /// Find the taskbar, create the child window, and dock it.
    pub fn attach(cfg: &Config) -> AttachResult<(Self, Notifier)> {
        use windows::Win32::Foundation::GetLastError;
        // SAFETY: every call here passes live locals or well-known strings.
        unsafe {
            let taskbar =
                find_taskbar().ok_or("no taskbar: Shell_TrayWnd not found".to_string())?;
            let notify =
                find_notify(taskbar).ok_or("no TrayNotifyWnd inside the taskbar".to_string())?;

            let module =
                GetModuleHandleW(None).map_err(|e| format!("GetModuleHandleW: {e}"))?;
            let atom = register_class(HINSTANCE(module.0));
            let taskbar_created = RegisterWindowMessageW(w!("TaskbarCreated"));

            let dpi = {
                let d = GetDpiForWindow(taskbar);
                if d == 0 { DEFAULT_DPI } else { d }
            };

            let mut rect = RECT::default();
            GetWindowRect(taskbar, &mut rect).map_err(|e| format!("GetWindowRect(taskbar): {e}"))?;
            let height = rect.bottom - rect.top;

            let mut notify_rect = RECT::default();
            GetWindowRect(notify, &mut notify_rect)
                .map_err(|e| format!("GetWindowRect(TrayNotifyWnd): {e}"))?;

            // Shared with the telemetry thread, which formats every sample
            // through it. This is what makes a tile switched off in the
            // Settings page stop painting within a tick — a copy per side would
            // keep the old visibility until the next launch.
            let shared = Arc::new(Mutex::new(cfg.clone()));
            let (sender, receiver) = channel();
            let mut state = Box::new(WindowState {
                receiver,
                model: TrayModel::default(),
                renderer: Renderer::new(cfg, dpi),
                cfg: cfg.clone(),
                taskbar_created,
                icon: None,
                instance: HINSTANCE(module.0),
                ui: HWND::default(),
                telemetry: shared,
            });

            // Reserve width from a worst-case sample so changing digits never
            // force a resize — a taskbar layout change every second is worse
            // than a few pixels of slack.
            let width = state.renderer.needed_width(&worst_case(cfg)).max(1);
            let x = notify_rect.left - rect.left - width;

            // WS_EX_LAYERED is deliberately absent: Windows refuses to create a
            // layered child of Shell_TrayWnd (verified — CreateWindowExW fails
            // with an invalid-handle error the moment the flag is set). The
            // theme's transparency is handled by *not* painting a background
            // instead; see `Renderer::paint`.
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("ArboTrayMetric"),
                w!(""),
                WS_CHILD | WS_VISIBLE,
                x,
                0,
                width,
                height,
                Some(taskbar),
                None,
                Some(HINSTANCE(module.0)),
                Some(&*state as *const WindowState as *const core::ffi::c_void),
            )
            .map_err(|e| {
                // A class-not-found failure leaves GetLastError at the value the
                // register call set, so report it alongside the atom.
                format!(
                    "CreateWindowExW: {e} (class atom={atom}, last_error={})",
                    GetLastError().0
                )
            })?;

            // The icon can only be added once the window exists to receive its
            // clicks. Its menu is the only way to quit, so a refusal here is
            // fatal — tear the half-built window down rather than run
            // unquittable.
            match Icon::install(hwnd, HINSTANCE(module.0), &state.model.tooltip()) {
                Ok(icon) => state.icon = Some(icon),
                Err(e) => {
                    let _ = DestroyWindow(hwnd);
                    return Err(format!("tray icon: {e}"));
                }
            }

            // The window owns one handle to the shared config, the notifier the
            // other for the telemetry thread.
            let notifier = Notifier {
                sender,
                hwnd,
                config: Arc::clone(&state.telemetry),
            };
            Ok((Tray { hwnd, state }, notifier))
        }
    }

    /// Run the message loop. Blocks until `WM_QUIT`.
    pub fn message_loop(&mut self) -> AttachResult<()> {
        let mut msg = MSG::default();
        // SAFETY: `msg` is a live local; the loop ends on WM_QUIT, which only
        // our own `WndProc` posts.
        unsafe {
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        Ok(())
    }

    /// Tear the window down. Safe to call more than once.
    pub fn shutdown(&self) {
        // SAFETY: DestroyWindow on our own live window; a second call is a
        // no-op because the handle is no longer valid and the call fails.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }
}

/// `Shell_TrayWnd` — the main taskbar window.
fn find_taskbar() -> Option<HWND> {
    // SAFETY: both names are static, null-terminated literals.
    unsafe { FindWindowW(w!("Shell_TrayWnd"), None).ok() }
}

/// `TrayNotifyWnd` — the clock / notification area inside the taskbar.
fn find_notify(taskbar: HWND) -> Option<HWND> {
    // SAFETY: `taskbar` came from FindWindowW and the class name is a literal.
    unsafe { FindWindowExW(Some(taskbar), None, w!("TrayNotifyWnd"), None).ok() }
}

/// Register our window class.
///
/// A zero return means either a real failure or "already registered" (a second
/// instance, or a restart after Explorer crashed). The two are not worth
/// telling apart here — `CreateWindowExW` reports the only case that matters,
/// a class that genuinely does not exist, with a better message.
fn register_class(instance: HINSTANCE) -> u16 {
    let class = WNDCLASSW {
        lpfnWndProc: Some(wnd_proc),
        hInstance: instance,
        lpszClassName: w!("ArboTrayMetric"),
        // We paint every pixel, so no background brush.
        hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH::default(),
        ..Default::default()
    };
    // SAFETY: `class.lpszClassName` outlives the call and the struct is live.
    unsafe { RegisterClassW(&class) }
}

/// A model whose text is as wide as each enabled field is likely to get.
///
/// Reserved at attach so the window never has to resize per tick. Rates cap at
/// four digits plus a unit, percentages at three.
pub fn worst_case(cfg: &Config) -> TrayModel {
    let field = |on: bool, text: &str| if on { text.to_string() } else { String::new() };
    TrayModel {
        down_text: field(cfg.show.net_down, "999.9M/s"),
        up_text: field(cfg.show.net_up, "999.9M/s"),
        latency_text: field(cfg.show.latency, "9999ms"),
        cpu_text: field(cfg.show.cpu, "100%"),
        ram_text: field(cfg.show.ram, "100%"),
        // Empty on purpose: the gateway, internet and loss readings are Network
        // page detail with no tile of their own. Reserving width for text that
        // never draws would just make the taskbar wider for nothing.
        gateway_text: String::new(),
        internet_text: String::new(),
        loss_text: String::new(),
        wifi_text: field(cfg.show.wifi, "6G 100%"),
        wifi_name: None,
        usage_text: field(cfg.show.usage, "9999.9G"),
        quota_alert: false,
        history: if cfg.show.sparkline {
            vec![0; RESERVED_HISTORY]
        } else {
            Vec::new()
        },
    }
}

/// The application-defined message range shares a space with `WM_APP`.
/// Referencing it here keeps the intent explicit next to the class setup.
pub const APP_MESSAGE_BASE: u32 = WM_APP;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Show;

    #[test]
    fn worst_case_matches_the_configured_fields() {
        let cfg = Config {
            show: Show {
                net_down: true,
                net_up: false,
                latency: true,
                cpu: false,
                ram: true,
                wifi: false,
                usage: false,
                sparkline: false,
            },
            ..Default::default()
        };
        let m = worst_case(&cfg);
        assert_eq!(m.down_text, "999.9M/s");
        assert!(m.up_text.is_empty(), "disabled field must not reserve width");
        assert!(m.cpu_text.is_empty());
        assert_eq!(m.ram_text, "100%");
        assert!(m.history.is_empty(), "no sparkline, no reserved width");
    }

    #[test]
    fn worst_case_reserves_the_history_span_when_enabled() {
        let cfg = Config::default(); // sparkline on by default
        assert_eq!(worst_case(&cfg).history.len(), RESERVED_HISTORY);
    }

    #[test]
    fn app_messages_sit_in_the_reserved_range() {
        // WM_APP is 0x8000; anything lower would collide with a system message.
        assert_eq!(APP_MESSAGE_BASE, 0x8000);
        assert!(WM_TRAY_UPDATE > APP_MESSAGE_BASE);
    }
}
