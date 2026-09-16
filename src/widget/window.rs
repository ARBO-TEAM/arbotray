//! The panel's window: creation, the message pump, dragging, and the paint.
//!
//! Modelled on the strip's `dock`, but a top-level window rather than a taskbar
//! child. That difference is the whole reason this exists: `WS_EX_LAYERED` is
//! refused by `Shell_TrayWnd`, so the strip has to sample a colour underneath
//! itself, while a top-level window takes a real alpha and can be dragged
//! anywhere on the desktop.
//!
//! It shares the strip's thread and its message loop, and repaints when the
//! strip does — so there is no second telemetry path, no second channel, and
//! nothing to keep in step between the two surfaces.

use super::metrics::{BLOCK_GAP, CLOSE, CLOSE_INSET, COL_GAP, MARGIN, PAD};
use super::render::Renderer;
use super::rows::{Role, Row, rows};
use crate::config::Config;
use crate::taskbar::TrayModel;
use std::ffi::c_void;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, ClientToScreen, CreateCompatibleBitmap, CreateCompatibleDC,
    CreateSolidBrush, DeleteDC, DeleteObject, EndPaint, FillRect, FrameRect, GetDC,
    GetMonitorInfoW, HGDIOBJ, InvalidateRect, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    MonitorFromWindow, PAINTSTRUCT, ReleaseDC, SRCCOPY, SelectObject, SetBkMode, SetTextColor,
    TRANSPARENT,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetClientRect,
    GetWindowLongPtrW, GetWindowRect, HTCAPTION, HTCLIENT, HWND_NOTOPMOST, HWND_TOPMOST, IDC_ARROW,
    IsWindowVisible, LWA_ALPHA, LoadCursorW, RegisterClassW, SW_HIDE, SW_SHOW, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetLayeredWindowAttributes, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, WM_CLOSE, WM_DESTROY, WM_ERASEBKGND, WM_LBUTTONUP, WM_NCCREATE,
    WM_NCHITTEST, WM_PAINT, WNDCLASSW, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_POPUP, WS_VISIBLE,
};
use windows::core::w;

/// A rectangle in screen coordinates.
///
/// Screen rather than client, because that is the space `WM_NCHITTEST` gives
/// its coordinates in, and converting once per paint beats converting once per
/// mouse move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct CloseBox {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl CloseBox {
    fn hit(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

/// State behind the panel's `WndProc`.
struct WidgetState {
    renderer: Renderer,
    /// The rows on screen, so a repaint draws what the last sample produced
    /// rather than re-deriving it, and the hit test knows where the close box
    /// landed without laying the panel out again.
    rows: Vec<Row>,
    close: CloseBox,
}

/// The panel, or the fact that there is not one.
///
/// Holds the state the window procedure reaches through `GWLP_USERDATA`, so it
/// must outlive the window: [`Widget::destroy`] is the only way to take it down.
pub struct Widget {
    hwnd: HWND,
    state: Box<WidgetState>,
}

impl Widget {
    /// Create the panel from the first sample, or `None` if Windows refuses the
    /// window — which the caller reads as "the widget is off", not as an error
    /// worth taking the app down for.
    pub fn create(cfg: &Config, model: &TrayModel, instance: HINSTANCE) -> Option<Self> {
        register_class(instance);
        let renderer = Renderer::new(cfg, dpi_of_desktop());
        let rows = rows(model, &cfg.widget);
        let (w, h) = renderer.size(&rows);
        let (x, y) = placement(cfg, w, h);
        let state = Box::new(WidgetState {
            renderer,
            rows,
            close: CloseBox::default(),
        });

        // SAFETY: `state` is a live `Box` moved into the returned `Widget`, so
        // it outlives the window it is handed to, and the class name is a
        // literal. The class is registered just above.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_LAYERED,
                w!("ArboTrayWidget"),
                w!("ArboTray"),
                WS_POPUP | WS_VISIBLE,
                x,
                y,
                w,
                h,
                None,
                None,
                Some(instance),
                Some(&*state as *const WidgetState as *const c_void),
            )
        }
        .ok()?;

        let me = Self { hwnd, state };
        me.style(cfg, false);
        Some(me)
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub fn is_visible(&self) -> bool {
        // SAFETY: our own window; the call only reads visibility.
        unsafe { IsWindowVisible(self.hwnd) }.as_bool()
    }

    /// Show or hide without destroying, so the panel keeps its position and the
    /// toggle in Settings is instant. Whether it is *meant* to be on is the
    /// user's setting, owned by the Settings page, not by this window.
    pub fn set_visible(&self, visible: bool) {
        // SAFETY: our own live window.
        unsafe {
            let _ = ShowWindow(self.hwnd, if visible { SW_SHOW } else { SW_HIDE });
        }
    }

    /// Take the window down.
    ///
    /// Clears the state pointer first, so nothing the teardown itself sends can
    /// reach a `WidgetState` that has since moved.
    pub fn destroy(&mut self) {
        // SAFETY: our own live window; a second call fails harmlessly.
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.hwnd);
        }
    }

    /// Repaint from a new sample, resizing only when the layout's own size
    /// changed — a panel that resized every tick would twitch as digits moved.
    pub fn update(&mut self, model: &TrayModel, cfg: &Config) {
        let old = self.state.renderer.size(&self.state.rows);
        self.state.rows = rows(model, &cfg.widget);
        let new = self.state.renderer.size(&self.state.rows);
        // SAFETY: our own live window.
        unsafe {
            if old != new {
                let _ = SetWindowPos(
                    self.hwnd,
                    None,
                    0,
                    0,
                    new.0,
                    new.1,
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// Rebuild fonts, rows and geometry after a theme or widget edit.
    ///
    /// `resize` is separate because a config that changed only the alpha has no
    /// reason to move the window, and moving it would undo a drag the user just
    /// finished for no visible gain.
    pub fn apply(&mut self, cfg: &Config, model: &TrayModel, resize: bool) {
        self.state.renderer = Renderer::new(cfg, dpi_of_desktop());
        self.state.rows = rows(model, &cfg.widget);
        self.style(cfg, resize);
    }

    /// Alpha, top-most, and the size when the layout may have changed.
    fn style(&self, cfg: &Config, resize: bool) {
        let (w, h) = self.state.renderer.size(&self.state.rows);
        // SAFETY: our own live window; the handle outlives the call.
        unsafe {
            let _ = SetLayeredWindowAttributes(self.hwnd, COLORREF(0), alpha(cfg), LWA_ALPHA);
            let insert = Some(if cfg.widget.always_on_top {
                HWND_TOPMOST
            } else {
                HWND_NOTOPMOST
            });
            let _ = SetWindowPos(self.hwnd, insert, 0, 0, w, h, style_flags(resize));
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// Where the user parked the panel, so it comes back there.
    ///
    /// Read back from the window rather than tracked during the drag: Windows
    /// moves a caption window itself, so it is the only party that knows where
    /// the panel ended up.
    pub fn position(&self) -> Option<(f64, f64)> {
        let mut r = RECT::default();
        // SAFETY: our own live window; `r` is a live local.
        unsafe {
            GetWindowRect(self.hwnd, &mut r).ok()?;
        }
        Some((f64::from(r.left), f64::from(r.top)))
    }
}

/// The flags `style` restyles with.
///
/// `SWP_NOMOVE` is in **both** cases, and it is the only reason this is a
/// function instead of an expression: the x/y arguments below are placeholders,
/// so a call without it teleports the panel to the top-left corner — which is
/// exactly what it did the first time, on the `create` call that runs before
/// anyone has moved it. `SWP_NOZORDER` is in neither, so the top-most choice
/// actually takes effect.
///
/// Split out because a winit-free test cannot press a window's buttons, but it
/// can assert the one flag whose absence is invisible until you look at where
/// the panel ended up.
fn style_flags(resize: bool) -> windows::Win32::UI::WindowsAndMessaging::SET_WINDOW_POS_FLAGS {
    if resize {
        SWP_NOMOVE | SWP_NOACTIVATE
    } else {
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE
    }
}

/// The alpha a panel with no opacity set of its own opens at.
///
/// Deliberately see-through rather than opaque: the panel is meant to sit on the
/// desktop without taking it over, and the readings stay legible because the
/// background goes translucent *with* the text, which keeps the contrast between
/// them — a light grey on black reads the same at 74% as at 100%, only dimmer.
/// `LWA_ALPHA` fades the whole window, so this is the setting where the desktop
/// shows through and the numbers still hold; below about 150 they start to
/// disappear against a busy wallpaper.
const DEFAULT_ALPHA: u8 = 190;

/// The panel's alpha.
///
/// `opacity: 0` means "sample the taskbar" for the strip, but there is nothing
/// to sample behind a floating window, so it reads as the panel's own
/// translucent default. Any non-zero value is the user's and is taken as given,
/// so one config still means one thing per surface.
fn alpha(cfg: &Config) -> u8 {
    if cfg.theme.opacity == 0 {
        DEFAULT_ALPHA
    } else {
        cfg.theme.opacity
    }
}

/// Where the panel goes: its remembered corner, or the top-right of the work
/// area on a first run.
///
/// Clamped into the work area either way, because a remembered corner can
/// outlive the monitor it was on — a saved position from a display that is no
/// longer attached would otherwise park the panel off-screen with no way back.
fn placement(cfg: &Config, w: i32, h: i32) -> (i32, i32) {
    let work = work_area();
    let (x, y) = match (cfg.widget.x, cfg.widget.y) {
        (Some(x), Some(y)) if x.is_finite() && y.is_finite() => (x.round() as i32, y.round() as i32),
        _ => (work.right - w - MARGIN, work.top + MARGIN),
    };
    (
        x.clamp(work.left, (work.right - w).max(work.left)),
        y.clamp(work.top, (work.bottom - h).max(work.top)),
    )
}

/// The work area of the monitor nearest the cursor, which is the one the panel
/// opens on.
fn work_area() -> RECT {
    // SAFETY: both calls take a live local and write into it.
    unsafe {
        let monitor = MonitorFromWindow(HWND::default(), MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            return info.rcWork;
        }
    }
    // A monitor query that fails is not a reason to refuse to draw: the usual
    // screen is a better guess than no window.
    RECT {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1080,
    }
}

/// The DPI the panel opens at. A window that has not picked a monitor yet has
/// no per-window DPI, so the system's is the right one to build fonts from.
fn dpi_of_desktop() -> u32 {
    // SAFETY: a null window asks for the system DPI.
    let dpi = unsafe { GetDpiForWindow(HWND::default()) };
    if dpi == 0 { 96 } else { dpi }
}

/// Register the class. A second call fails, which is exactly what a second
/// widget creation should do here.
fn register_class(instance: HINSTANCE) {
    // SAFETY: `class` is a live local; every pointer in it is null or a
    // well-known constant that outlives the call.
    unsafe {
        let class = WNDCLASSW {
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hInstance: instance,
            lpszClassName: w!("ArboTrayWidget"),
            lpfnWndProc: Some(wnd_proc),
            ..Default::default()
        };
        let _ = RegisterClassW(&class);
    }
}

/// # Safety
/// Registered as the class's `lpfnWndProc`; `hwnd` is therefore one of ours,
/// which is what guarantees `GWLP_USERDATA` holds a live `*mut WidgetState`.
unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            let create = lparam.0 as *const CREATESTRUCTW;
            if !create.is_null() {
                let state = (*create).lpCreateParams as *mut WidgetState;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
                // The pointer is stashed; let creation continue.
                return LRESULT(1);
            }
        }

        let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WidgetState;
        if state.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let state = &mut *state;

        match msg {
            // The panel has no title bar, so the whole surface answers as one
            // and Windows moves the window for us — including the snapping a
            // hand-rolled drag would have to reimplement badly. The close box is
            // the one exception: it answers as client area, so the click arrives
            // as `WM_LBUTTONUP` below instead of starting a drag. The
            // coordinates in an `NCHITTEST` are screen-space, which is why the
            // box is stored that way.
            WM_NCHITTEST => {
                let x = low_word_signed(lparam.0 as usize);
                let y = high_word_signed(lparam.0 as usize);
                if state.close.hit(x, y) {
                    return LRESULT(HTCLIENT as isize);
                }
                LRESULT(HTCAPTION as isize)
            }

            // Only reachable inside the close box, because everything else is
            // caption. Hide rather than destroy: the panel keeps its position,
            // and the Settings checkbox stays the single record of whether the
            // widget is meant to be on. The caption's own system menu and Alt+F4
            // arrive as `WM_CLOSE`, so there is no second path to keep in step.
            WM_LBUTTONUP | WM_CLOSE => {
                let _ = ShowWindow(hwnd, SW_HIDE);
                LRESULT(0)
            }

            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let _ = BeginPaint(hwnd, &mut ps);
                state.paint(hwnd);
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }

            // Fully painted on every frame, so a background erase is one flicker
            // of work for nothing.
            WM_ERASEBKGND => LRESULT(1),

            // Deliberately NOT `PostQuitMessage`: this window shares the strip's
            // message loop, and quitting it would take the app down with it.
            WM_DESTROY => LRESULT(0),

            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// The signed low and high halves of an `LPARAM`, which is how mouse messages
/// pack a coordinate pair. Signed because the halves are two's complement, and a
/// multi-monitor desktop is full of negative coordinates.
pub fn low_word_signed(v: usize) -> i32 {
    (v & 0xFFFF) as u16 as i16 as i32
}

pub fn high_word_signed(v: usize) -> i32 {
    ((v >> 16) & 0xFFFF) as u16 as i16 as i32
}

impl WidgetState {
    fn paint(&mut self, hwnd: HWND) {
        // SAFETY: `hwnd` is our live window; every GDI object created here is
        // selected back out or deleted before returning, and the DC is released.
        unsafe {
            let mut rect = RECT::default();
            if GetClientRect(hwnd, &mut rect).is_err() {
                return;
            }
            let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
            if w <= 0 || h <= 0 {
                return;
            }

            let dc = GetDC(Some(hwnd));
            if dc.is_invalid() {
                return;
            }
            let mem = CreateCompatibleDC(Some(dc));
            let bitmap = CreateCompatibleBitmap(dc, w, h);
            let old_bitmap = SelectObject(mem, HGDIOBJ(bitmap.0));

            // Drawn into a memory DC and blitted once, so a repaint at 1 Hz
            // never shows a half-finished frame.
            let brush = CreateSolidBrush(self.renderer.bg);
            FillRect(mem, &rect, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));

            // An edge, because the panel can sit over any wallpaper and a border
            // is what separates it from one the same colour.
            let edge = CreateSolidBrush(self.renderer.border);
            FrameRect(mem, &rect, edge);
            let _ = DeleteObject(HGDIOBJ(edge.0));

            let pad = self.renderer.scaled(PAD);
            let gap = self.renderer.scaled(COL_GAP);
            let col = self.renderer.label_column(&self.rows);
            let mut y = pad;
            let mut first = true;

            SetBkMode(mem, TRANSPARENT);
            for row in &self.rows {
                if row.role == Role::Title && !first {
                    y += self.renderer.scaled(BLOCK_GAP);
                }
                first = false;

                let old = SelectObject(mem, HGDIOBJ(self.renderer.font_for(row.role).0));
                if row.label.is_empty() {
                    SetTextColor(mem, self.renderer.fg);
                    self.renderer.text(mem, pad, y, &row.value);
                } else {
                    SetTextColor(mem, self.renderer.dim);
                    self.renderer.text(mem, pad, y, &row.label);
                    SetTextColor(
                        mem,
                        match row.role {
                            Role::Alert => self.renderer.alert,
                            _ => self.renderer.fg,
                        },
                    );
                    self.renderer.text(mem, pad + col + gap, y, &row.value);
                }
                SelectObject(mem, old);
                y += self.renderer.line_height(row.role);
            }

            // Last, so nothing is drawn over it, and recorded in screen space as
            // it is drawn — the hit test then reads exactly what is on screen
            // rather than a second guess at the same arithmetic.
            let side = self.renderer.scaled(CLOSE);
            let inset = self.renderer.scaled(CLOSE_INSET);
            let client = RECT {
                left: w - pad - side,
                top: inset,
                right: w - pad,
                bottom: inset + side,
            };
            self.close = to_screen(hwnd, client);

            let old = SelectObject(mem, HGDIOBJ(self.renderer.body.0));
            SetTextColor(mem, self.renderer.dim);
            self.renderer.text(
                mem,
                client.left + self.renderer.scaled(5),
                client.top,
                "\u{00D7}",
            );
            SelectObject(mem, old);

            let _ = BitBlt(dc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
            SelectObject(mem, old_bitmap);
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
            let _ = DeleteDC(mem);
            ReleaseDC(Some(hwnd), dc);
        }
    }
}

/// A client-space rectangle as screen space, so `WM_NCHITTEST` can compare.
fn to_screen(hwnd: HWND, r: RECT) -> CloseBox {
    let mut tl = POINT { x: r.left, y: r.top };
    let mut br = POINT {
        x: r.right,
        y: r.bottom,
    };
    // SAFETY: our own live window; both points are live locals.
    unsafe {
        let _ = ClientToScreen(hwnd, &mut tl);
        let _ = ClientToScreen(hwnd, &mut br);
    }
    CloseBox {
        left: tl.x,
        top: tl.y,
        right: br.x,
        bottom: br.y,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_coordinate_pair_survives_the_lparam_packing() {
        // Negative coordinates are ordinary on a multi-monitor desktop, so the
        // sign has to be recovered rather than the halves read as unsigned.
        let packed = ((5i32 as u32 & 0xFFFF) as usize) | (((-3i32 as u32 & 0xFFFF) as usize) << 16);
        assert_eq!(low_word_signed(packed), 5);
        assert_eq!(high_word_signed(packed), -3);
    }

    #[test]
    fn a_close_box_answers_only_for_itself() {
        let b = CloseBox {
            left: 100,
            top: 50,
            right: 120,
            bottom: 70,
        };
        assert!(b.hit(100, 50));
        assert!(b.hit(119, 69));
        assert!(!b.hit(120, 70), "right/bottom edge is outside, as RECT says");
        assert!(!b.hit(99, 60));
    }

    #[test]
    fn a_restyle_never_moves_the_panel() {
        // The regression this exists for: `style` passes 0,0 as the x/y
        // arguments because it only ever means to resize, re-order or re-alpha.
        // A call without `SWP_NOMOVE` therefore parks the panel in the corner —
        // which is what happened on the `create` call, before the user had a
        // chance to drag it anywhere.
        for resize in [true, false] {
            let flags = style_flags(resize).0;
            assert!(
                flags & SWP_NOMOVE.0 != 0,
                "resize={resize} may not move the window"
            );
            assert_eq!(
                flags & windows::Win32::UI::WindowsAndMessaging::SWP_NOZORDER.0,
                0,
                "resize={resize} must let the top-most choice through"
            );
        }
        // And the size only rides along on the call that means it.
        assert_eq!(style_flags(true).0 & SWP_NOSIZE.0, 0);
        assert_ne!(style_flags(false).0 & SWP_NOSIZE.0, 0);
    }

    #[test]
    fn an_unset_opacity_gives_the_panel_its_own_translucency() {
        // Not 255: the panel is meant to show the desktop through it, and not 0
        // either, which `LWA_ALPHA` would read as "invisible" — the two failures
        // this sits between.
        let mut cfg = Config::default();
        cfg.theme.opacity = 0;
        assert_eq!(alpha(&cfg), DEFAULT_ALPHA);
        assert!(DEFAULT_ALPHA > 0 && DEFAULT_ALPHA < 255);
        cfg.theme.opacity = 128;
        assert_eq!(alpha(&cfg), 128, "an explicit opacity is the user's");
    }

    #[test]
    fn a_remembered_corner_is_used_when_the_screen_allows_it() {
        // The fallback is the work area's top-right; a corner already inside it
        // must be taken as given, or a panel dragged somewhere deliberate would
        // snap back on the next launch.
        let mut cfg = Config::default();
        cfg.widget.x = Some(10.0);
        cfg.widget.y = Some(10.0);
        assert_eq!(placement(&cfg, 200, 100), (10, 10));
    }

    #[test]
    fn a_corner_from_a_detached_monitor_is_pulled_back_on_screen() {
        // The failure this guards: a position saved on a second display that is
        // no longer attached would park the panel where nothing can reach it.
        let mut cfg = Config::default();
        cfg.widget.x = Some(-90_000.0);
        cfg.widget.y = Some(90_000.0);
        let (x, y) = placement(&cfg, 200, 100);
        assert!(x >= 0 && y >= 0, "got ({x},{y})");
    }

    #[test]
    fn a_first_run_lands_inside_the_work_area() {
        let cfg = Config::default();
        let (x, y) = placement(&cfg, 260, 400);
        let work = work_area();
        assert!(x >= work.left && x + 260 <= work.right, "x={x}");
        assert!(y >= work.top && y + 400 <= work.bottom, "y={y}");
    }
}
