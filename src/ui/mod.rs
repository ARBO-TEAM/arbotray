//! The dashboard window.
//!
//! A normal top-level window — caption, minimise box, resize frame — that the
//! tray shows when clicked. It exists because the taskbar strip has room for a
//! handful of numbers and nothing else: the network name, today's total and the
//! sparkline's history have nowhere to go out there.
//!
//! Layout: a page list down the left, the selected page on the right. The list
//! is a plain `LISTBOX` — a built-in user32 class, so no new dependency and no
//! owner-draw plumbing — and it is themed through `WM_CTLCOLORLISTBOX`. Eight
//! numbers do not need a sidebar, but they were already crowding one flat
//! column, and every feature left in the list wants a page to live on.
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
    InvalidateRect, NULL_BRUSH, OUT_DEFAULT_PRECIS, PAINTSTRUCT, PS_SOLID, Polyline, ReleaseDC,
    SelectObject, SetBkColor, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA,
    GetClientRect, GetSystemMetrics, GetWindowLongPtrW, HMENU, IsIconic, IsWindow, LB_ADDSTRING,
    LB_ERR, LB_GETCURSEL, LB_SETCURSEL, LBS_NOTIFY, LBS_NOINTEGRALHEIGHT, LBN_SELCHANGE, MINMAXINFO,
    MoveWindow, RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_RESTORE, SW_SHOW,
    SWP_NOACTIVATE, SWP_NOZORDER, SendMessageW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLORLISTBOX, WM_DPICHANGED, WM_ERASEBKGND,
    WM_GETMINMAXINFO, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SETFONT, WM_SIZE, WNDCLASSW,
    WINDOW_EX_STYLE, WINDOW_STYLE, WS_BORDER, WS_CHILD, WS_CLIPCHILDREN, WS_OVERLAPPEDWINDOW,
    WS_VISIBLE, WS_VSCROLL,
};
use windows::core::{PCWSTR, w};

/// The window class, registered on first show and reused after.
const CLASS: PCWSTR = w!("ArboTrayWindow");

/// Child id of the page list, handed to `CreateWindowExW` as an `HMENU` and
/// read back out of the low word of `WM_COMMAND`'s `wparam`.
const LIST_ID: i32 = 1;

/// The pages, in the order the list shows them. Their indices are the page
/// numbers used throughout, so the constants below name the slots rather than
/// leaving magic numbers in the row functions.
const PAGES: [&str; 4] = ["Overview", "Network", "System", "Data"];
const OVERVIEW: usize = 0;
const NETWORK: usize = 1;
const SYSTEM: usize = 2;
const DATA: usize = 3;

/// Metrics in 96-DPI pixels. The font and the sidebar width carry the scaling;
/// these are the ratios everything else is laid out from.
const PAD: i32 = 20;
const ROW_H: i32 = 30;
const VALUE_OFFSET: i32 = 6;
const SPARK_GAP: i32 = 14;
const SIDEBAR_W: i32 = 150;
const TITLE_EXTRA: i32 = 6;

/// Smallest size still showing every row without the frame collapsing. Wider
/// than the single-column window was, because the sidebar eats its share.
const MIN_W: i32 = 520;
const MIN_H: i32 = 320;

/// Initial size: room for the rows plus a decent sparkline.
const START_W: i32 = 720;
const START_H: i32 = 460;

/// What the window knows between repaints. Boxed and hung off the window's
/// `GWLP_USERDATA`, so there is no global and no lifetime to get wrong.
struct UiState {
    model: TrayModel,
    cfg: Config,
    font: HFONT,
    /// Value face — one step heavier, so the numbers lead and the labels
    /// annotate.
    bold: HFONT,
    /// Page heading, a few points up from the body.
    title: HFONT,
    /// The page list. A child window, so it paints itself; this handle is only
    /// for laying it out and reading its selection.
    list: HWND,
    /// Which page is showing. Kept here rather than only in the listbox so a
    /// frame can be painted without asking the child anything.
    page: usize,
    /// Interface scale, from `WM_DPICHANGED`. Everything laid out by hand is
    /// multiplied by this.
    dpi: u32,
    /// The sidebar's face, held because `WM_CTLCOLORLISTBOX` has to hand the
    /// same brush back on every one of the list's paints.
    side_brush: HBRUSH,
    /// Our own module, for creating the child window.
    instance: HINSTANCE,
}

/// Show the window, creating it the first time. `existing` is the caller's
/// remembered handle; an invalid one means "not up yet".
pub fn ensure(instance: HINSTANCE, cfg: &Config, hint: &TrayModel, existing: HWND) -> Option<HWND> {
    // SAFETY: `existing` is only tested, never dereferenced.
    if !existing.is_invalid() && unsafe { IsWindow(Some(existing)) }.as_bool() {
        return Some(existing);
    }

    let bg = background(cfg);
    let state = Box::new(UiState {
        model: hint.clone(),
        cfg: cfg.clone(),
        font: create_font(cfg, 96, 0, false),
        bold: create_font(cfg, 96, 1, true),
        title: create_font(cfg, 96, TITLE_EXTRA, true),
        list: HWND::default(),
        page: OVERVIEW,
        dpi: 96,
        // Owned by the window from here, same as the fonts: `WM_NCDESTROY`
        // deletes it.
        side_brush: unsafe { CreateSolidBrush(shade(bg, 18)) },
        instance,
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
            // `WS_CLIPCHILDREN` keeps the parent's repaints out of the sidebar,
            // which is what stops the list flickering once per sample.
            WINDOW_STYLE(WS_OVERLAPPEDWINDOW.0 | WS_CLIPCHILDREN.0),
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
        // The content area only: the sidebar does not change with a sample.
        let _ = InvalidateRect(Some(hwnd), None, false);
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

// --- pages ----------------------------------------------------------------

/// The rows for one page, in order, skipping anything switched off.
///
/// Splitting them is the point of the sidebar: the traffic numbers are what
/// people open this for, so they lead the overview, and the adapter and
/// hardware detail moves one click away instead of burying them.
fn page_rows(page: usize, model: &TrayModel) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let mut push = |label: &'static str, text: &str| {
        if !text.is_empty() {
            out.push((label, text.to_string()));
        }
    };
    match page {
        NETWORK => {
            if let Some(name) = &model.wifi_name {
                push("Network", name);
            }
            push("Wi-Fi", &model.wifi_text);
            push("Download", &model.down_text);
            push("Upload", &model.up_text);
            push("Latency", &model.latency_text);
        }
        SYSTEM => {
            push("CPU", &model.cpu_text);
            push("RAM", &model.ram_text);
        }
        DATA => {
            push("Today", &model.usage_text);
        }
        // OVERVIEW, and the fallback for an index that cannot happen: showing
        // the traffic is always better than showing nothing.
        _ => {
            push("Download", &model.down_text);
            push("Upload", &model.up_text);
            push("Latency", &model.latency_text);
            push("CPU", &model.cpu_text);
            push("RAM", &model.ram_text);
        }
    }
    out
}

/// Whether a page has anything for the sparkline to say. The traffic history
/// belongs with the traffic figures, and the daily total is the same story at
/// a coarser grain — the hardware pages have no history to draw.
fn page_shows_graph(page: usize) -> bool {
    matches!(page, OVERVIEW | DATA)
}

// --- colours --------------------------------------------------------------

/// The window's own background, or a neutral dark when the theme's value is
/// unparseable.
fn background(cfg: &Config) -> COLORREF {
    parse_color(&cfg.theme.background).unwrap_or(COLORREF(0x0020_2020))
}

/// One step away from `bg`, in whichever direction the theme goes: lighter on
/// a dark theme, darker on a light one. This is what lets the sidebar separate
/// itself from the content without a second colour in the config file.
fn shade(bg: COLORREF, step: u32) -> COLORREF {
    let (r, g, b) = (bg.0 & 0xFF, (bg.0 >> 8) & 0xFF, (bg.0 >> 16) & 0xFF);
    // Perceived luminance, so mid-greys pick the sane side.
    let luma = (r * 299 + g * 587 + b * 114) / 1000;
    let f = |v: u32| {
        if luma < 128 {
            (v + step).min(255)
        } else {
            v.saturating_sub(step)
        }
    };
    // COLORREF is 0x00BBGGRR, not RGB.
    COLORREF(f(r) | (f(g) << 8) | (f(b) << 16))
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
            // Before the first paint, so nothing is ever drawn into a window
            // that has no sidebar to lay out.
            WM_CREATE => {
                if !state.is_null() {
                    let s = &mut *state;
                    create_sidebar(hwnd, s);
                    layout(hwnd, s);
                }
                LRESULT(0)
            }

            WM_SIZE => {
                if !state.is_null() {
                    layout(hwnd, &mut *state);
                }
                LRESULT(0)
            }

            // The list tells us when the user picks a page. `LB_SETCURSEL` —
            // how the initial page is chosen — does not raise this, so there
            // is no risk of the two paths fighting.
            WM_COMMAND => {
                if !state.is_null()
                    && low_word(wparam.0) as i32 == LIST_ID
                    && high_word(wparam.0) as u32 == LBN_SELCHANGE
                {
                    let s = &mut *state;
                    if !s.list.is_invalid() {
                        let picked = SendMessageW(s.list, LB_GETCURSEL, None, None).0 as i32;
                        if picked != LB_ERR && picked >= 0 {
                            s.page = picked as usize;
                            let _ = InvalidateRect(Some(hwnd), None, false);
                        }
                    }
                }
                LRESULT(0)
            }

            // The list is a system control and would paint itself grey with a
            // white well. Hand it the theme instead: this paints each item's
            // background, and the returned brush fills the empty space below
            // the last row.
            WM_CTLCOLORLISTBOX => {
                if !state.is_null() {
                    let s = &*state;
                    let dc = windows::Win32::Graphics::Gdi::HDC(wparam.0 as *mut core::ffi::c_void);
                    let _ = SetBkColor(dc, shade(background(&s.cfg), 18));
                    let _ = SetTextColor(dc, parse_color(&s.cfg.theme.foreground).unwrap_or(fg_default()));
                    return LRESULT(s.side_brush.0 as isize);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }

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
                    for font in [&mut s.font, &mut s.bold, &mut s.title] {
                        let _ = DeleteObject(HGDIOBJ(font.0));
                    }
                    s.dpi = if dpi == 0 { 96 } else { dpi };
                    s.font = create_font(&s.cfg, s.dpi, 0, false);
                    s.bold = create_font(&s.cfg, s.dpi, 1, true);
                    s.title = create_font(&s.cfg, s.dpi, TITLE_EXTRA, true);
                    // The list has its own font, so it has to be told too.
                    layout(hwnd, s);

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
                    (*info).ptMinTrackSize.x = scale(MIN_W, if state.is_null() { 96 } else { (*state).dpi });
                    (*info).ptMinTrackSize.y = scale(MIN_H, if state.is_null() { 96 } else { (*state).dpi });
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
                    let _ = DeleteObject(HGDIOBJ(s.title.0));
                    let _ = DeleteObject(HGDIOBJ(s.side_brush.0));
                }
                LRESULT(0)
            }

            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// Build the page list. A `LISTBOX` rather than anything we draw: it already
/// knows about selection, keyboard navigation and scrolling, and it is part of
/// user32, so it costs nothing to ship.
fn create_sidebar(parent: HWND, state: &mut UiState) {
    // SAFETY: `parent` is the window being created, mid-`WM_CREATE`; the class
    // name is a static literal and the strings outlive their `SendMessageW`.
    unsafe {
        let style = WINDOW_STYLE(
            WS_CHILD.0
                | WS_VISIBLE.0
                | WS_VSCROLL.0
                | WS_BORDER.0
                // The `LBS_` flags are plain `i32` in the bindings while the
                // `WS_` ones are a newtype, hence the mixed casts.
                | LBS_NOTIFY as u32
                | LBS_NOINTEGRALHEIGHT as u32,
        );
        let list = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("LISTBOX"),
            PCWSTR::null(),
            style,
            0,
            0,
            sidebar_w(state.dpi),
            0,
            Some(parent),
            // The child id arrives as an `HMENU`, which is how `WM_COMMAND`
            // knows which control spoke.
            Some(HMENU(LIST_ID as *mut core::ffi::c_void)),
            Some(state.instance),
            None,
        );
        let Ok(list) = list else {
            // No sidebar, but the window still works: the content paints from
            // x=0 and every page is reachable — just not switchable.
            return;
        };
        state.list = list;

        for name in PAGES {
            let mut wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            SendMessageW(
                list,
                LB_ADDSTRING,
                None,
                Some(LPARAM(wide.as_mut_ptr() as isize)),
            );
        }
        SendMessageW(list, LB_SETCURSEL, Some(WPARAM(state.page)), None);
    }
}

/// Put the sidebar where it belongs and give it the current font. Called on
/// every resize, which is also what keeps it correct across a DPI change.
fn layout(hwnd: HWND, state: &mut UiState) {
    if state.list.is_invalid() {
        return;
    }
    // SAFETY: `list` was checked, and both `SetWindowPos` and `SendMessageW`
    // only touch our own child.
    unsafe {
        let mut rect = RECT::default();
        if GetClientRect(hwnd, &mut rect).is_err() {
            return;
        }
        let h = rect.bottom - rect.top;
        if h <= 0 {
            return;
        }
        let _ = SetWindowPos(
            state.list,
            None,
            0,
            0,
            sidebar_w(state.dpi),
            h,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
        SendMessageW(
            state.list,
            WM_SETFONT,
            Some(WPARAM(state.font.0 as usize)),
            // `lparam` non-zero means "redraw now".
            Some(LPARAM(1)),
        );
    }
}

/// Draw the content area: background, heading, rows, sparkline.
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
        let bg = background(&state.cfg);
        let fg = if state.model.quota_alert {
            parse_color(&state.cfg.theme.alert).unwrap_or(COLORREF(0x0000_00FF))
        } else {
            parse_color(&state.cfg.theme.foreground).unwrap_or(fg_default())
        };

        let side = sidebar_w(state.dpi);
        let pad = scale(PAD, state.dpi);
        let row_h = scale(ROW_H, state.dpi);
        // Where the content column starts. When the list could not be created
        // this collapses to the plain single-column layout.
        let x0 = if state.list.is_invalid() { pad } else { side + pad };
        let x1 = w - pad;
        if x1 <= x0 {
            return;
        }

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

        // The divider sits at the sidebar's edge rather than inside it, so it
        // survives `WS_CLIPCHILDREN` clipping the list's rectangle out.
        if !state.list.is_invalid() {
            let line = shade(bg, 44);
            let pen = CreatePen(PS_SOLID, 1, line);
            let old = SelectObject(dc, HGDIOBJ(pen.0));
            let edge = [POINT { x: side, y: 0 }, POINT { x: side, y: h }];
            let _ = Polyline(dc, &edge);
            SelectObject(dc, old);
            let _ = DeleteObject(HGDIOBJ(pen.0));
        }

        let page = state.page.min(PAGES.len() - 1);
        let value_offset = scale(VALUE_OFFSET, state.dpi);
        // The heading sits at the padding itself; the rows are nudged down into
        // their bands, which is what makes a label and its value look level.
        let title_h = scale(ROW_H + TITLE_EXTRA * 2, state.dpi);
        draw(dc, state.title, PAGES[page], x0, pad, x1, DT_LEFT);
        let mut y = pad + title_h + value_offset;

        for (label, value) in &page_rows(page, &state.model) {
            draw(dc, state.font, label, x0, y, x1, DT_LEFT);
            draw(dc, state.bold, value, x0, y, x1, DT_RIGHT);
            y += row_h;
        }

        // The sparkline fills whatever room is left, so the picture grows with
        // the window instead of sitting in a fixed corner.
        let spark_gap = scale(SPARK_GAP, state.dpi);
        let spark_top = y + spark_gap;
        let spark_w = x1 - x0;
        let spark_h = h - spark_gap - pad - spark_top;
        if page_shows_graph(page) && spark_h >= 8 && spark_w > 0 && !state.model.history.is_empty() {
            let points = sparkline_points(&state.model.history, spark_w, spark_h);
            if points.len() > 1 {
                let moved: Vec<POINT> = points
                    .into_iter()
                    .map(|p| POINT {
                        x: p.x + x0,
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

/// One single-line run of text, starting at `top`.
///
/// Single-line text is laid out from the top of the rectangle, so `top` is the
/// text's own top: callers add their own nudge for a row's band. The bottom is
/// deliberately generous — it can only clip, never reposition, so a heading
/// taller than a row shares this one function.
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
        top,
        right,
        bottom: top + ROW_H * 8,
    };
    // SAFETY: the font and DC are the caller's, both live; `wide` and `rect`
    // outlive the call.
    unsafe {
        SelectObject(dc, HGDIOBJ(font.0));
        DrawTextW(dc, &mut wide, &mut rect, DT_SINGLELINE | DT_NOPREFIX | align);
    }
}

/// Scale a 96-DPI metric to the current display, never to zero.
fn scale(value: i32, dpi: u32) -> i32 {
    (value * dpi as i32 / 96).max(1)
}

fn sidebar_w(dpi: u32) -> i32 {
    scale(SIDEBAR_W, dpi)
}

/// The theme's body colour when the configured one cannot be parsed.
fn fg_default() -> COLORREF {
    COLORREF(0x00E6_E6E6)
}

/// Building a row font at `dpi`. `extra` is added to the configured point size
/// — values lead, headings more so. A failed `CreateFontW` yields a null
/// `HFONT`, which GDI reads as "the default font" — degraded, not fatal.
fn create_font(cfg: &Config, dpi: u32, extra: i32, bold: bool) -> HFONT {
    let points = cfg.theme.font_size.max(9) as i32 + extra;
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

/// Low and high halves of a `WPARAM`, which is where `WM_COMMAND` packs the
/// child id and the notification code. `LOWORD`/`HIWORD` are not in the
/// bindings, and the masks are the whole of them.
fn low_word(value: usize) -> u16 {
    (value & 0xFFFF) as u16
}

fn high_word(value: usize) -> u16 {
    ((value >> 16) & 0xFFFF) as u16
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

    fn labels(page: usize, model: &TrayModel) -> Vec<&'static str> {
        page_rows(page, model).into_iter().map(|(l, _)| l).collect()
    }

    #[test]
    fn every_page_is_reachable_from_the_list() {
        // The list is built from `PAGES` and indexed by position, so a page
        // whose rows fall through to `_` would be silently invisible.
        for (i, name) in PAGES.iter().enumerate() {
            assert!(!name.is_empty(), "page {i} has no label to click");
        }
        assert_eq!(PAGES.first(), Some(&"Overview"), "the first page is the one shown");
        assert!(PAGES.len() > 1, "a one-page sidebar is not a sidebar");
    }

    #[test]
    fn the_overview_leads_with_the_traffic_numbers() {
        assert_eq!(
            labels(OVERVIEW, &full()),
            vec!["Download", "Upload", "Latency", "CPU", "RAM"]
        );
    }

    #[test]
    fn the_adapter_detail_moves_off_the_overview() {
        // The whole reason the network information is worth a page: it was
        // competing with the numbers people actually open the window for.
        let net = labels(NETWORK, &full());
        assert_eq!(net, vec!["Network", "Wi-Fi", "Download", "Upload", "Latency"]);
        assert!(!labels(OVERVIEW, &full()).contains(&"Network"));
    }

    #[test]
    fn the_system_page_is_only_hardware() {
        assert_eq!(labels(SYSTEM, &full()), vec!["CPU", "RAM"]);
    }

    #[test]
    fn the_daily_total_has_a_page_of_its_own() {
        let data = page_rows(DATA, &full());
        assert_eq!(data.len(), 1);
        assert_eq!(data[0], ("Today", "1.4G".to_string()));
        assert!(page_shows_graph(DATA), "the total is a traffic story too");
    }

    #[test]
    fn only_the_traffic_pages_draw_the_graph() {
        assert!(page_shows_graph(OVERVIEW));
        assert!(!page_shows_graph(NETWORK));
        assert!(!page_shows_graph(SYSTEM));
    }

    #[test]
    fn empty_fields_leave_no_blank_rows() {
        let only_down = TrayModel {
            down_text: "1.4M/s".into(),
            ..Default::default()
        };
        assert_eq!(page_rows(OVERVIEW, &only_down).len(), 1);

        // A switched-off tile is empty, but the latency placeholder is not.
        let placeholder = TrayModel {
            latency_text: "--".into(),
            ..Default::default()
        };
        let rows = page_rows(OVERVIEW, &placeholder);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, "--");
    }

    #[test]
    fn a_window_with_nothing_switched_on_has_no_rows() {
        for page in 0..PAGES.len() {
            assert!(page_rows(page, &TrayModel::default()).is_empty());
        }
    }

    #[test]
    fn the_command_packing_is_unpacked_the_way_windows_packs_it() {
        // `WM_COMMAND` low word is the child id, high word the notification.
        let packed = (LBN_SELCHANGE as usize) << 16 | LIST_ID as usize;
        assert_eq!(low_word(packed) as i32, LIST_ID);
        assert_eq!(high_word(packed) as u32, LBN_SELCHANGE);
        // A different control's notification must not switch our page.
        assert_ne!(low_word((LBN_SELCHANGE as usize) << 16 | 7) as i32, LIST_ID);
    }

    #[test]
    fn the_sidebar_shifts_with_the_theme_not_against_it() {
        // Dark theme: the sidebar has to be lighter, or it disappears.
        let dark = shade(COLORREF(0x0000_0000), 18);
        assert_eq!(dark.0, 0x0012_1212);

        // Light theme: darker, same reason.
        let light = shade(COLORREF(0x00FF_FFFF), 18);
        assert_eq!(light.0, 0x00ED_EDED);

        // COLORREF is BGR, so the channels must move together.
        let c = shade(COLORREF(0x0000_0000), 18).0;
        assert_eq!(c & 0xFF, (c >> 8) & 0xFF);
        assert_eq!(c & 0xFF, (c >> 16) & 0xFF);
    }

    #[test]
    fn shading_never_wraps_past_black_or_white() {
        assert_eq!(shade(COLORREF(0x0000_0000), 300).0, 0x00FF_FFFF);
        assert_eq!(shade(COLORREF(0x00FF_FFFF), 300).0, 0x0000_0000);
    }

    #[test]
    fn the_minimum_size_is_smaller_than_the_initial_size() {
        // The frame must be draggable smaller than it starts, or the floor is
        // a lie and the window looks stuck.
        assert!(MIN_H < START_H);
        assert!(MIN_W < START_W);
        assert!(MIN_H >= 200, "a floor under 200px is not a usable window");
        // The minimum has to leave room for the sidebar plus a value column.
        assert!(MIN_W > SIDEBAR_W * 2, "the floor would leave no content area");
    }

    #[test]
    fn scaled_metrics_never_collapse_to_nothing() {
        assert_eq!(scale(SIDEBAR_W, 96), SIDEBAR_W);
        assert_eq!(scale(SIDEBAR_W, 144), 225);
        // A garbage DPI would otherwise multiply the whole layout by zero.
        assert_eq!(scale(SIDEBAR_W, 0), 1);
        assert!(sidebar_w(192) > sidebar_w(96));
    }
}
