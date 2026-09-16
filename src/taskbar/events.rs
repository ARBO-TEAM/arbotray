//! The tray window's `WndProc`.
//!
//! The window is created with a `*mut WindowState` in `lpCreateParams`; the
//! first message (`WM_NCCREATE`) stashes it in `GWLP_USERDATA` so every later
//! message can reach the receiver and renderer without a global.

use crate::config::Config;
use crate::taskbar::render::Renderer;
use crate::taskbar::TrayModel;
use std::sync::mpsc::Receiver;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, DefWindowProcW, GWLP_USERDATA, GetClientRect, GetWindowLongPtrW, MoveWindow,
    PostQuitMessage, SetWindowLongPtrW, WM_APP, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND,
    WM_NCCREATE, WM_PAINT,
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
            // Explorer restarted and our parent taskbar is gone. We cannot
            // rebuild ourselves from inside our own doomed message loop, so
            // quit and let the supervisor in `app` re-attach to the new one.
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
                let _ = InvalidateRect(Some(hwnd), None, false);
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
                PostQuitMessage(0);
                LRESULT(0)
            }

            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// Low 16 bits of a `WPARAM`, which is where `WM_DPICHANGED` packs the new DPI.
pub fn low_word(value: usize) -> u16 {
    (value & 0xFFFF) as u16
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
