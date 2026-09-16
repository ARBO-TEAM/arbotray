//! The dashboard window.
//!
//! A normal top-level window — caption, minimise box, resize frame — that the
//! tray shows when clicked. It exists because the taskbar strip has room for a
//! handful of numbers and nothing else: the network name, today's total and the
//! sparkline's history have nowhere to go out there.
//!
//! It lives on the tray's own thread and is driven by direct calls rather than
//! messages: same thread, so a sample is written straight into the window's
//! state — no queue, no locking, no chance of a stale post.
//!
//! Closing it hides it. The taskbar display is the product; the window is a
//! detail, and quitting on close would make a glance at the numbers fatal.

use crate::config::Config;
use crate::taskbar::TrayModel;
use crate::taskbar::icon::app_icon;
use crate::taskbar::render::{parse_color, sparkline_points};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreatePen, CreateSolidBrush,
    DEFAULT_CHARSET, DEFAULT_GUI_FONT, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DeleteObject,
    DrawTextW, EndPaint, FW_NORMAL, FillRect, GetDC, GetStockObject, HBRUSH, HGDIOBJ, HFONT,
    NULL_BRUSH, OUT_DEFAULT_PRECIS, PAINTSTRUCT, PS_SOLID, Polyline, ReleaseDC, SelectObject,
    SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA,
    GetClientRect, GetSystemMetrics, GetWindowLongPtrW, IsIconic, IsWindow, MINMAXINFO, MoveWindow,
    RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_RESTORE, SW_SHOW, SetForegroundWindow,
    SetWindowLongPtrW, ShowWindow, WM_CLOSE, WM_DPICHANGED, WM_ERASEBKGND, WM_GETMINMAXINFO,
    WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WNDCLASSW, WINDOW_EX_STYLE, WS_OVERLAPPEDWINDOW,
};
use windows::core::{PCWSTR, w};

/// The window class, registered on first show and reused after.
const CLASS: PCWSTR = w!("ArboTrayWindow");

/// Padding and row metrics, in 96-DPI pixels; the font carries the scaling.
const PAD: i32 = 14;
const ROW_H: i32 = 26;
const VALUE_OFFSET: i32 = 5;
const SPARK_GAP: i32 = 10;

/// Smallest size still showing every row without the frame collapsing.
const MIN_W: i32 = 260;
const MIN_H: i32 = 240;

/// Initial size: room for the rows plus a decent sparkline.
const START_W: i32 = 400;
const START_H: i32 = 320;

/// What the window knows between repaints. Boxed and hung off the window's
/// `GWLP_USERDATA`, so there is no global and no lifetime to get wrong.
struct UiState {
    model: TrayModel,
    cfg: Config,
    font: HFONT,
    /// Value face — one step heavier, so the numbers lead and the labels
    /// annotate.
    bold: HFONT,
}

/// Show the window, creating it the first time. `existing` is the caller's
/// remembered handle; an invalid one means "not up yet".
pub fn ensure(instance: HINSTANCE, cfg: &Config, hint: &TrayModel, existing: HWND) -> Option<HWND> {
    // SAFETY: `existing` is only tested, never dereferenced.
    if !existing.is_invalid() && unsafe { IsWindow(Some(existing)) }.as_bool() {
        return Some(existing);
    }

    let state = Box::new(UiState {
        model: hint.clone(),
        cfg: cfg.clone(),
        font: create_font(cfg, 96, false),
        bold: create_font(cfg, 96, true),
    });
    // Handed to the window, which owns it from here: `WM_NCDESTROY` turns this
    // back into a `Box` and drops it. Freeing it here instead would leave
    // `GWLP_USERDATA` pointing at released memory, which reads as a corrupt
    // model rather than as a crash — the strings grow nonsense lengths and the
    // next allocation fails.
    let state = Box::into_raw(state);

    // SAFETY: the class name and icon are static or process-owned, and `state`
    // is handed to the window, which reclaims it in `WM_NCDESTROY`.
    let hwnd = unsafe {
        let class = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: instance,
            lpszClassName: CLASS,
            hIcon: app_icon(instance),
            // Every pixel is painted below. A class brush would flash the
            // wrong colour through every resize.
            hbrBackground: HBRUSH::default(),
            ..Default::default()
        };
        // A second registration simply fails, which is the outcome we want.
        let _ = RegisterClassW(&class);

        let screen_w = GetSystemMetrics(SM_CXSCREEN);
        let screen_h = GetSystemMetrics(SM_CYSCREEN);
        let x = if screen_w > START_W {
            (screen_w - START_W) / 2
        } else {
            CW_USEDEFAULT
        };
        let y = if screen_h > START_H {
            (screen_h - START_H) / 2
        } else {
            CW_USEDEFAULT
        };

        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            w!("ArboTray"),
            WS_OVERLAPPEDWINDOW,
            x,
            y,
            START_W,
            START_H,
            None,
            None,
            Some(instance),
            Some(state as *const core::ffi::c_void),
        )
        .ok()?
    };

    Some(hwnd)
}

/// Bring the window up, restoring it if it was minimised.
pub fn show(hwnd: HWND) {
    // SAFETY: `hwnd` is our own live window.
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        } else {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        let _ = SetForegroundWindow(hwnd);
    }
}

/// Hand the window the newest sample and repaint it. A no-op when the window
/// is not up, so the tray can call it unconditionally once per tick.
pub fn update(hwnd: HWND, model: &TrayModel) {
    if hwnd.is_invalid() {
        return;
    }
    // SAFETY: same thread as the window, so the state pointer cannot race
    // anyone. `IsWindow` covers a handle that has been torn down.
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            return;
        }
        let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut UiState;
        if state.is_null() {
            return;
        }
        (*state).model = model.clone();
        let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, false);
    }
}

/// Destroy the window if it is up. Called at shutdown so the process does not
/// linger behind a visible frame.
pub fn close(hwnd: HWND) {
    if hwnd.is_invalid() {
        return;
    }
    // SAFETY: our own window; `IsWindow` guards a handle already torn down.
    unsafe {
        if IsWindow(Some(hwnd)).as_bool() {
            let _ = DestroyWindow(hwnd);
        }
    }
}

/// The label/value rows, in order, skipping anything switched off.
///
/// Download and upload appear at the same precision the taskbar uses; the
/// network name gets a row of its own because that is the whole reason this
/// window exists.
fn rows(model: &TrayModel) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let mut push = |label: &'static str, text: &str| {
        if !text.is_empty() {
            out.push((label, text.to_string()));
        }
    };
    push("Download", &model.down_text);
    push("Upload", &model.up_text);
    push("Latency", &model.latency_text);
    push("CPU", &model.cpu_text);
    push("RAM", &model.ram_text);
    push("Wi-Fi", &model.wifi_text);
    push("Today", &model.usage_text);
    if let Some(name) = &model.wifi_name {
        push("Network", name);
    }
    out
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            let create = lparam.0 as *const CREATESTRUCTW;
            if !create.is_null() {
                let state = (*create).lpCreateParams as *mut UiState;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
                return LRESULT(1);
            }
        }

        let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut UiState;

        match msg {
            WM_PAINT => {
                if state.is_null() {
                    return DefWindowProcW(hwnd, msg, wparam, lparam);
                }
                let mut ps = PAINTSTRUCT::default();
                let _ = BeginPaint(hwnd, &mut ps);
                paint(hwnd, &*state);
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }

            // Painted edge to edge below; letting the default erase first is
            // exactly the flicker this avoids.
            WM_ERASEBKGND => LRESULT(1),

            WM_DPICHANGED => {
                if !state.is_null() {
                    let dpi = (wparam.0 & 0xFFFF) as u32;
                    let s = &mut *state;
                    for font in [&mut s.font, &mut s.bold] {
                        let _ = DeleteObject(HGDIOBJ(font.0));
                    }
                    s.font = create_font(&s.cfg, dpi, false);
                    s.bold = create_font(&s.cfg, dpi, true);

                    let suggested = lparam.0 as *const RECT;
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
                }
                LRESULT(0)
            }

            WM_GETMINMAXINFO => {
                // Without a floor the frame drags down to a sliver and every
                // row clips — broken-looking, and no one asked for it.
                if lparam.0 != 0 {
                    let info = lparam.0 as *mut MINMAXINFO;
                    (*info).ptMinTrackSize.x = MIN_W;
                    (*info).ptMinTrackSize.y = MIN_H;
                }
                LRESULT(0)
            }

            // Hide, do not destroy: the taskbar display keeps sampling either
            // way, and rebuilding the window on every glance is churn.
            WM_CLOSE => {
                let _ = ShowWindow(hwnd, SW_HIDE);
                LRESULT(0)
            }

            WM_NCDESTROY => {
                if !state.is_null() {
                    // Clear the slot first so a late message cannot reach
                    // freed memory.
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    let s = Box::from_raw(state);
                    let _ = DeleteObject(HGDIOBJ(s.font.0));
                    let _ = DeleteObject(HGDIOBJ(s.bold.0));
                }
                LRESULT(0)
            }

            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// Draw the whole window: background, rows, sparkline.
fn paint(hwnd: HWND, state: &UiState) {
    // SAFETY: every GDI object created here is deleted before returning or
    // restored into the DC it came from, and the DC is released.
    unsafe {
        let mut rect = RECT::default();
        if GetClientRect(hwnd, &mut rect).is_err() {
            return;
        }
        let w = rect.right - rect.left;
        let h = rect.bottom - rect.top;
        if w <= 0 || h <= 0 {
            return;
        }

        // The window is opaque, so unlike the taskbar strip there is no
        // sampling trick here — the theme's background is used as given.
        let bg = parse_color(&state.cfg.theme.background).unwrap_or(COLORREF(0x0020_2020));
        let fg = if state.model.quota_alert {
            parse_color(&state.cfg.theme.alert).unwrap_or(COLORREF(0x0000_00FF))
        } else {
            parse_color(&state.cfg.theme.foreground).unwrap_or(COLORREF(0x00E6_E6E6))
        };

        // One DC for the whole frame: `FillRect` and the glyphs both go to it,
        // so the background cannot land a frame behind the text.
        let dc = GetDC(Some(hwnd));
        if dc.is_invalid() {
            return;
        }
        let brush = CreateSolidBrush(bg);
        FillRect(dc, &rect, brush);
        let _ = DeleteObject(HGDIOBJ(brush.0));

        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, fg);

        let items = rows(&state.model);
        let mut y = PAD;
        for (label, value) in &items {
            draw(dc, state.font, label, PAD, y, w - PAD, DT_LEFT);
            draw(dc, state.bold, value, PAD, y, w - PAD, DT_RIGHT);
            y += ROW_H;
        }

        // The sparkline fills whatever room is left, so the picture grows with
        // the window instead of sitting in a fixed corner.
        let spark_top = y + SPARK_GAP;
        let spark_w = w - PAD * 2;
        let spark_h = h - SPARK_GAP - PAD - spark_top;
        if spark_h >= 8 && spark_w > 0 && !state.model.history.is_empty() {
            let points = sparkline_points(&state.model.history, spark_w, spark_h);
            if points.len() > 1 {
                let moved: Vec<POINT> = points
                    .into_iter()
                    .map(|p| POINT {
                        x: p.x + PAD,
                        y: p.y + spark_top,
                    })
                    .collect();
                let pen = CreatePen(PS_SOLID, 1, fg);
                let old = SelectObject(dc, HGDIOBJ(pen.0));
                let _ = Polyline(dc, &moved);
                SelectObject(dc, old);
                let _ = DeleteObject(HGDIOBJ(pen.0));
            }
        }

        // Leave the DC holding stock objects rather than ours.
        SelectObject(dc, GetStockObject(NULL_BRUSH));
        SelectObject(dc, GetStockObject(DEFAULT_GUI_FONT));
        ReleaseDC(Some(hwnd), dc);
    }
}

/// One single-line run of text, vertically centred in a `ROW_H` band.
///
/// # Safety
/// `dc` must be a live DC and `font` a live font.
unsafe fn draw(
    dc: windows::Win32::Graphics::Gdi::HDC,
    font: HFONT,
    text: &str,
    left: i32,
    top: i32,
    right: i32,
    align: windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT,
) {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    let mut rect = RECT {
        left,
        top: top + VALUE_OFFSET,
        right,
        bottom: top + ROW_H,
    };
    // SAFETY: the font and DC are the caller's, both live; `wide` and `rect`
    // outlive the call.
    unsafe {
        SelectObject(dc, HGDIOBJ(font.0));
        DrawTextW(dc, &mut wide, &mut rect, DT_SINGLELINE | DT_NOPREFIX | align);
    }
}

/// Build a row font at `dpi`. A failed `CreateFontW` yields a null `HFONT`,
/// which GDI reads as "the default font" — degraded, not fatal.
fn create_font(cfg: &Config, dpi: u32, bold: bool) -> HFONT {
    let points = cfg.theme.font_size.max(9) as i32 + if bold { 1 } else { 0 };
    let height = -(points * dpi as i32 / 96);
    // SAFETY: the face name is a static literal; everything else is by value.
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            if bold { 700 } else { FW_NORMAL.0 as i32 },
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            w!("Segoe UI"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full() -> TrayModel {
        TrayModel {
            down_text: "1.4M/s".into(),
            up_text: "0.2M/s".into(),
            latency_text: "8ms".into(),
            cpu_text: "12%".into(),
            ram_text: "44%".into(),
            wifi_text: "5G 78%".into(),
            wifi_name: Some("HomeNet".into()),
            usage_text: "1.4G".into(),
            quota_alert: false,
            history: vec![1, 2, 3],
        }
    }

    #[test]
    fn every_populated_field_becomes_a_row() {
        let rows = rows(&full());
        let labels: Vec<&str> = rows.iter().map(|(l, _)| *l).collect();
        assert_eq!(
            labels,
            vec![
                "Download", "Upload", "Latency", "CPU", "RAM", "Wi-Fi", "Today", "Network"
            ]
        );
        assert_eq!(rows[7].1, "HomeNet", "the name is the reason for the window");
    }

    #[test]
    fn empty_fields_leave_no_blank_rows() {
        let only_down = TrayModel {
            down_text: "1.4M/s".into(),
            ..Default::default()
        };
        assert_eq!(rows(&only_down).len(), 1);

        // A switched-off tile is empty, but the latency placeholder is not.
        let placeholder = TrayModel {
            latency_text: "--".into(),
            ..Default::default()
        };
        let rows = rows(&placeholder);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, "--");
    }

    #[test]
    fn a_window_with_nothing_switched_on_has_no_rows() {
        assert!(rows(&TrayModel::default()).is_empty());
    }

    #[test]
    fn the_minimum_height_is_smaller_than_the_initial_height() {
        // The frame must be draggable smaller than it starts, or the floor is
        // a lie and the window looks stuck.
        assert!(MIN_H < START_H);
        assert!(MIN_W < START_W);
        assert!(MIN_H >= 200, "a floor under 200px is not a usable window");
    }
}
