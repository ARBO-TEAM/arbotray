//! The tray window's `WndProc`.
//!
//! The window is created with a `*mut WindowState` in `lpCreateParams`; the
//! first message (`WM_NCCREATE`) stashes it in `GWLP_USERDATA` so every later
//! message can reach the receiver and renderer without a global.

use crate::config::Config;
use crate::taskbar::alert::Alerts;
use crate::taskbar::icon::{self, CMD_OPEN, CMD_QUIT, Icon, show_menu};
use crate::taskbar::render::Renderer;
use crate::taskbar::{TrayModel, dock};
use crate::telemetry::SpeedTest;
use crate::ui;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetClientRect, GetWindowLongPtrW,
    MoveWindow, PostQuitMessage, SetWindowLongPtrW, WM_APP, WM_DESTROY, WM_DPICHANGED,
    WM_ERASEBKGND, WM_LBUTTONUP, WM_NCCREATE, WM_PAINT, WM_RBUTTONUP,
};

/// Posted by the telemetry thread to wake the loop with a fresh sample.
pub const WM_TRAY_UPDATE: u32 = WM_APP + 1;

/// Everything the window needs to repaint itself.
pub struct WindowState {
    pub receiver: Receiver<TrayModel>,
    pub model: TrayModel,
    pub renderer: Renderer,
    pub cfg: Config,
    /// `RegisterWindowMessageW("TaskbarCreated")` — sent when Explorer restarts.
    pub taskbar_created: u32,
    /// `None` only between window creation and icon install.
    pub icon: Option<Icon>,
    /// Our own module, for the icon resource and the dashboard's window class.
    pub instance: HINSTANCE,
    /// The dashboard, created on first use and invalid until then. It lives on
    /// this thread, so it is written to directly rather than messaged.
    pub ui: HWND,
    /// The same config the telemetry thread formats with, shared rather than
    /// copied. A copy would mean a tile switched off in the Settings page kept
    /// painting until the next launch — the file would change and the taskbar
    /// would not, which is the exact failure this whole feature exists to fix.
    pub telemetry: Arc<Mutex<Config>>,
    /// The speed test, so the dashboard can start a run. This thread only ever
    /// hands it to the dashboard — it does not read the results, which arrive
    /// in the model through the telemetry thread's own clone of the handle.
    pub speed: SpeedTest,
    /// The desktop widget, when one is up.
    ///
    /// It lives here rather than inside the dashboard because it has to be
    /// created and destroyed from this thread — and because this is the window
    /// that already receives every sample, so the panel needs no channel of its
    /// own: it is repainted from the same model, in the same arm of the same
    /// message.
    pub widget: Option<crate::widget::Widget>,
    /// Decides which samples earn a balloon notification. Held across ticks
    /// because the rules are all about what already happened — a fresh value
    /// each sample would announce the same 90% once a second.
    pub alerts: Alerts,
}

/// Window procedure for the docked tray child.
///
/// # Safety
/// Registered as the window class's `lpfnWndProc`; `hwnd` must be a window of
/// that class, which is what guarantees `GWLP_USERDATA` holds a live
/// `*mut WindowState`.
pub unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        // The very first message: adopt the state pointer before anything can
        // ask for it. Returning 1 means "carry on creating the window".
        if msg == WM_NCCREATE {
            let create = lparam.0 as *const CREATESTRUCTW;
            if !create.is_null() {
                let state = (*create).lpCreateParams as *mut WindowState;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
                return LRESULT(1);
            }
        }

        let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowState;
        if state.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let state = &mut *state;

        if msg == state.taskbar_created {
            // Explorer restarted and our parent taskbar is gone. The child
            // window cannot outlive its parent, and we cannot rebuild
            // ourselves from inside our own doomed message loop, so the loop
            // is ended here and `app` returns.
            //
            // That means an Explorer restart ends the process: there is no
            // supervisor that re-attaches to the new taskbar yet, so the tray
            // display stays gone until the app is started again. Re-attaching
            // is the fix, and it belongs in `app`, not here.
            PostQuitMessage(0);
            return LRESULT(0);
        }

        match msg {
            WM_TRAY_UPDATE => {
                // Drain everything queued and keep only the newest sample —
                // repainting intermediate frames would just be wasted work.
                while let Ok(model) = state.receiver.try_recv() {
                    state.model = model;
                }
                if let Some(icon) = &mut state.icon {
                    icon.set_tip(&state.model.tooltip());
                    if let Some(b) = state.alerts.poll(
                        &state.cfg.notify,
                        state.model.quota_pct,
                        state.model.rx_bps,
                        std::time::Instant::now(),
                    ) {
                        icon.balloon(&b);
                    }
                }
                // The dashboard is fed here rather than from the telemetry
                // thread: same thread as the window, so no queue is needed.
                //
                // It is also the one place a Settings-page edit can reach this
                // window: `update` returns a config when the user has saved
                // one, and adopting it here is what makes the change visible
                // without a restart. `set_metrics` rebuilds the font, which is
                // what a size or colour change needs, and the repaint below
                // redraws the tile run from the new config.
                if let Some(cfg) = ui::update(state.ui, &state.model) {
                    // The tray's own copy, then the telemetry thread's. The
                    // second is what a tile-visibility or refresh change needs:
                    // `TrayModel::from_metric` reads `show` and the poll loop
                    // reads `interval_ms`, and neither of those is visible
                    // anywhere on this thread.
                    state.cfg = cfg;
                    dock::share_config(&state.telemetry, &state.cfg);
                    state.renderer.set_metrics(&state.cfg, dpi_of(hwnd));
                    // The strip's width is reserved from `worst_case(cfg)`, so
                    // showing a tile that was hidden needs a wider window.
                    resize_to_fit(state, hwnd);
                    // The panel is built, destroyed or restyled here for the
                    // same reason the strip is: this is the one place a
                    // Settings-page edit reaches this thread.
                    sync_widget(state);
                }
                if let Some(widget) = &mut state.widget {
                    widget.update(&state.model, &state.cfg);
                }
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }

            // Left click opens the dashboard — the normal thing a click on an
            // app's face does. Right click gets the menu, which is where
            // quitting lives.
            WM_LBUTTONUP => {
                open_dashboard(state);
                LRESULT(0)
            }

            WM_RBUTTONUP => {
                menu(state, hwnd);
                LRESULT(0)
            }

            // The tray icon is a separate window from the system's point of
            // view, so its clicks arrive as `WM_TRAY_ICON` with the mouse
            // message packed into the low word of `lparam` — not as the
            // `WM_*BUTTONUP` above. Same two gestures, same handler.
            icon::WM_TRAY_ICON => {
                match low_word(lparam.0 as usize) as u32 {
                    WM_LBUTTONUP => open_dashboard(state),
                    WM_RBUTTONUP => menu(state, hwnd),
                    _ => {}
                }
                LRESULT(0)
            }

            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let _ = BeginPaint(hwnd, &mut ps);
                state.renderer.paint(hwnd, &state.model, &state.cfg);
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }

            // We paint every pixel ourselves in `paint`; letting the default
            // handler erase first is what causes flicker.
            WM_ERASEBKGND => LRESULT(1),

            WM_DPICHANGED => {
                let dpi = low_word(wparam.0) as u32;
                state.renderer.set_metrics(&state.cfg, dpi);
                // lparam points at the rect Windows suggests for the new DPI.
                let suggested = lparam.0 as *const windows::Win32::Foundation::RECT;
                if !suggested.is_null() {
                    let r = *suggested;
                    let _ = MoveWindow(
                        hwnd,
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        true,
                    );
                }
                LRESULT(0)
            }

            WM_DESTROY => {
                // Take the dashboard down with us. Leaving a top-level window
                // behind an exiting process would strand it on screen.
                ui::close(state.ui);
                // The panel too, and before the state that backs it is freed:
                // the window procedure reaches its paint state through a
                // pointer into that box, so the window has to go first.
                if let Some(mut widget) = state.widget.take() {
                    widget.destroy();
                }
                PostQuitMessage(0);
                LRESULT(0)
            }

            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// Create the dashboard if it does not exist yet, then raise it.
///
/// The window is built lazily rather than at startup: most of the time nobody
/// clicks, and an app that puts a window on screen before it is asked to is
/// the reason people uninstall tray tools.
fn open_dashboard(state: &mut WindowState) {
    if let Some(hwnd) = ui::ensure(
        state.instance,
        &state.cfg,
        &state.model,
        state.ui,
        state.speed.clone(),
    ) {
        state.ui = hwnd;
        ui::show(hwnd);
    }
}

/// Bring the desktop widget into line with the config that was just saved.
///
/// Three cases, and no fourth: nothing to do when the setting has not moved, a
/// build when it has just been switched on, and a teardown when it has just
/// been switched off. A panel already up takes its new alpha and colours in
/// place, and only moves when the layout's own size changed — the position is
/// the user's, and a theme edit is not a reason to undo a drag.
fn sync_widget(state: &mut WindowState) {
    match (state.cfg.widget.enabled, state.widget.is_some()) {
        (true, false) => {
            state.widget = crate::widget::Widget::create(&state.cfg, &state.model, state.instance);
        }
        (false, true) => {
            if let Some(mut widget) = state.widget.take() {
                widget.destroy();
            }
        }
        (true, true) => {
            if let Some(widget) = &mut state.widget {
                // `true` because a saved config can have changed the font size,
                // which is the one setting that changes the layout's own size.
                widget.apply(&state.cfg, &state.model, true);
                // Shown again, because the close box hides the panel without
                // touching the setting — so without this a panel dismissed
                // earlier could only be recovered by unticksing and reticking
                // the box. Landing on Save is a clear enough "show me again".
                widget.set_visible(true);
            }
        }
        (false, false) => {}
    }
}

/// Show the right-click menu and act on the choice. The menu is the only route
/// to Exit, so it is reachable from both the taskbar strip and the tray icon.
fn menu(state: &mut WindowState, hwnd: HWND) {
    match show_menu(hwnd) {
        Some(CMD_OPEN) => open_dashboard(state),
        Some(CMD_QUIT) => {
            // Destroying tears down the icon in `WindowState::drop` and posts
            // the `WM_QUIT` that ends the message loop.
            ui::close(state.ui);
            // SAFETY: `hwnd` is our own live window.
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        }
        // `None` is the menu being dismissed, which is not a command.
        _ => {}
    }
}

/// Low 16 bits of a `WPARAM`, which is where `WM_DPICHANGED` packs the new DPI.
pub fn low_word(value: usize) -> u16 {
    (value & 0xFFFF) as u16
}

/// The window's current DPI, or 96 when it cannot be read. `set_metrics` takes
/// the DPI as well as the config, and re-reading it here rather than caching it
/// means a settings edit on a display that was scaled after launch still gets
/// the right font.
fn dpi_of(hwnd: HWND) -> u32 {
    // SAFETY: `hwnd` is our own live window; the call reads a scalar.
    let dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd) };
    if dpi == 0 { 96 } else { dpi }
}

/// Widen the strip to whatever the new config's worst-case sample needs.
///
/// The window's width is reserved once at attach from `worst_case(cfg)`, so a
/// tile switched on in the Settings page would paint into a strip sized for the
/// tiles that were on at launch — clipped, with no way to notice. Only the
/// width moves: the dock position and the height belong to the taskbar.
fn resize_to_fit(state: &WindowState, hwnd: HWND) {
    let width = state.renderer.needed_width(&dock::worst_case(&state.cfg)).max(1);
    let mut rect = windows::Win32::Foundation::RECT::default();
    // SAFETY: `hwnd` is our own live window.
    unsafe {
        if windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut rect).is_err() {
            return;
        }
        if rect.right - rect.left == width {
            return;
        }
        let _ = MoveWindow(hwnd, rect.left, rect.top, width, rect.bottom - rect.top, true);
    }
}

/// Width of the window's client area, or 0 when it cannot be read.
pub fn client_width(hwnd: HWND) -> i32 {
    // SAFETY: `hwnd` is a live window and `rect` is a live local.
    unsafe {
        let mut rect = windows::Win32::Foundation::RECT::default();
        if GetClientRect(hwnd, &mut rect).is_err() {
            return 0;
        }
        rect.right - rect.left
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpi_is_read_from_the_low_word() {
        // 144 DPI (150%) packed into a WPARAM alongside an x/y pair.
        assert_eq!(low_word(144), 144);
        assert_eq!(low_word(96), 96);
        // High bits must not leak into the DPI.
        assert_eq!(low_word((200 << 16) | 144), 144);
    }

    #[test]
    fn the_update_message_does_not_collide_with_system_messages() {
        // It lives in the WM_APP range, which is reserved for applications.
        assert!(WM_TRAY_UPDATE >= 0x8000);
        assert_ne!(WM_TRAY_UPDATE, WM_PAINT);
        assert_ne!(WM_TRAY_UPDATE, WM_DESTROY);
    }
}
