//! The dashboard window.
//!
//! A normal top-level window — caption, minimise box, resize frame — that the
//! tray shows when clicked. It exists because the taskbar strip has room for a
//! handful of numbers and nothing else: the network name, today's total and the
//! sparkline's history have nowhere to go out there.
//!
//! Layout: a page list down the left, the selected page on the right. The list
//! is painted by the window itself rather than built from a system control, so
//! each entry can carry a glyph and the selection can be a rounded pill instead
//! of a highlight bar. `sidebar` owns it; this file only routes the clicks,
//! keys and hover to it. Eight numbers do not need a sidebar, but they were
//! already crowding one flat column, and every feature left in the list wants a
//! page to live on.
//!
//! It lives on the tray's own thread and is driven by direct calls rather than
//! messages: same thread, so a sample is written straight into the window's
//! state — no queue, no locking, no chance of a stale post.
//!
//! Closing it hides it. The taskbar display is the product; the window is a
//! detail, and quitting on close would make a glance at the numbers fatal.

//! The tree mirrors the file this came from: the window's lifecycle lives here,
//! the theme, fonts and metrics have a module each, and the pages, painter,
//! sidebar and settings controls sit beside them.

mod chart;
mod components;
mod consts;
mod design;
mod fonts;
mod layout;
mod modal;
pub(crate) use modal::{Action, Confirm, Modal, Outcome};
mod pages;
mod paint;
mod settings;
mod sidebar;
mod stopwatch;
mod theme;

pub(crate) use consts::*;
pub(crate) use fonts::*;
pub(crate) use layout::*;
pub(crate) use pages::*;
pub(crate) use paint::*;
pub(crate) use stopwatch::Stopwatch;
pub(crate) use settings::*;
pub(crate) use theme::*;
// `sidebar` is deliberately not glob-re-exported: its `paint` would collide
// with the frame painter's, and the window procedure names it explicitly.

use crate::config::Config;
use crate::power;
use crate::taskbar::TrayModel;
use crate::taskbar::icon::app_icon;
// Named because the tests build one: production code only ever reads the list.
#[cfg(test)]
use crate::taskbar::OpenPort;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, SYSTEMTIME, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_ROUND, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, HBRUSH, HGDIOBJ, HFONT, InvalidateRect,
    PAINTSTRUCT, SetBkColor, SetTextColor,
};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForSystem};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA,
    GetClientRect, GetSystemMetrics, GetWindowLongPtrW, IsIconic, IsWindow, KillTimer, MINMAXINFO,
    MoveWindow, RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_RESTORE, SW_SHOW,
    SetForegroundWindow, SetTimer, SetWindowLongPtrW, ShowWindow, WM_CLOSE, WM_COMMAND, WM_CREATE,
    WM_CTLCOLORBTN, WM_CTLCOLOREDIT, WM_CTLCOLORSTATIC, WM_DPICHANGED, WM_ERASEBKGND,
    WM_GETMINMAXINFO, WM_KEYDOWN, WM_LBUTTONDOWN, WM_MOUSEMOVE, WM_NCCREATE, WM_NCDESTROY,
    WM_PAINT, WM_SIZE, WM_TIMER, WNDCLASSW, WINDOW_EX_STYLE, WINDOW_STYLE, WS_CLIPCHILDREN,
    WS_OVERLAPPEDWINDOW,
};
use crate::telemetry::SpeedTest;
use windows::core::PCWSTR;

pub(crate) struct UiState {
    model: TrayModel,
    cfg: Config,
    font: HFONT,
    /// Value face — one step heavier, so the numbers lead and the labels
    /// annotate.
    bold: HFONT,
    /// Page heading, a few points up from the body.
    title: HFONT,
    /// The Stopwatch page's reading — the body face scaled up hard, because on
    /// that page the reading is the entire content and a clock the height of a
    /// row is not one you can read across a desk.
    clock: HFONT,
    /// Which page is showing. The sidebar is painted from this, so it is the
    /// only record of the selection there is.
    page: usize,
    /// The sidebar entry the pointer is over, if any. Held between messages
    /// because the hover pill is a pixel the *previous* frame drew: without it
    /// a repaint would either lose the highlight or leave one behind.
    hover: Option<usize>,
    /// Whether `TrackMouseEvent` is already armed for this visit.
    ///
    /// It fires once and then stops, so it has to be re-armed every time the
    /// pointer moves within the window — but only when it is not already
    /// pending, or a smooth mouse would ask the system to track it on every
    /// pixel of travel.
    tracking: bool,
    /// Interface scale, from `WM_DPICHANGED`. Everything laid out by hand is
    /// multiplied by this.
    dpi: u32,
    /// The content face, for the same reason: the settings page's checkboxes
    /// and edit fields ask their parent for a background brush on every paint
    /// of their own, and a control that is handed the wrong one shows a grey
    /// plate on a themed page.
    face_brush: HBRUSH,
    /// Our own module, for creating the child window.
    instance: HINSTANCE,
    /// What the Settings page was last loaded with. Held for the same reason
    /// the controls' values are not mirrored per keystroke: this is the
    /// comparison "did the user change anything", and it is only rebuilt at
    /// load and after a write.
    settings: SettingsForm,
    /// Nothing in the page is live until Save runs, so the page has to say so.
    /// A notice under the button is the whole feedback channel — a dialog would
    /// be a second window to own for one line of text.
    notice: Option<String>,
    /// Set when a save has replaced `cfg` and the tray has not been told yet.
    /// `update` clears it as it hands the config over. See `save_settings`.
    handed_back: bool,
    /// The speed test, for the Speed Test page's Run button. A handle to the
    /// same run the telemetry thread reports on, so a test started here shows
    /// up in the model within one tick.
    speed: SpeedTest,
    /// The last `speed_running` a sample carried, so the Run button is only
    /// told about a change rather than on every tick. A `SendMessage` per
    /// second to a control nobody is looking at is worth not doing.
    prev_running: bool,
    /// Which open port the user has clicked on the Ports page, as the pid that
    /// holds it.
    ///
    /// The pid and not an index, because the list is rebuilt every tick and a
    /// port that closes takes its row with it — every row under it then slides
    /// up into a different index, so an index-based selection would end up
    /// highlighting a port nobody clicked. A pid survives that: it is a
    /// property of the row rather than of where the row happened to sit, and it
    /// is what the Stop action needs anyway.
    ///
    /// The list dedupes by port, so one pid can hold two rows. That is why this
    /// is resolved by position on every use — `selected_index` — rather than
    /// trusted to name exactly one row.
    selected_port: Option<u32>,
    /// The confirmation popup, if one is up. Held here rather than as a child
    /// window so the frame can dim behind it in the same paint pass.
    modal: Modal,
    /// The bands the Ports page's open-port rows were last drawn on, each with
    /// the pid it was drawn from. Recorded by the painter and read by the hit
    /// test.
    ///
    /// A layout cache rather than arithmetic repeated in two places: the rows
    /// are laid out by walking a `Canvas`, whose cursor depends on how many
    /// metric rows above them were filled, so a second copy of that sum would
    /// be a second answer to "which row did I just click".
    ///
    /// The pid is carried *beside* the rectangle rather than looked up by index
    /// afterwards, because the list scrolls: a band's position in this vector
    /// is no longer the port's position in the model, and a click resolved
    /// through that offset would aim the Stop button at whatever slid into the
    /// band rather than at what is drawn in it.
    port_rows: Vec<(RECT, u32)>,
    /// How far down the open-port list the Ports page is scrolled, in rows.
    ///
    /// A dev machine holds more listeners than fit a page, and the one the
    /// reader came for — the server they just started — is as likely to be
    /// below the fold as above it. Clamped in the painter, against the room
    /// that page actually has, so this cannot outlive a shorter list.
    port_scroll: usize,
    /// What the last force-stop did, painted at the foot of the Ports page.
    /// Separate from `notice`, which belongs to the Settings page: a save's
    /// confirmation has no business appearing over a list of sockets.
    port_notice: Option<String>,
    /// The Stopwatch page's clock, and the reason one page of this window
    /// repaints on its own schedule rather than once per sample.
    ///
    /// It counts in real time while a run is going, so a tick that arrives a
    /// second late is a tick a whole second of the display is missing. See
    /// `TICK_MS`: the tray is asked to feed this window faster while it runs,
    /// and the timer is never touched when it is not.
    watch: Stopwatch,
    /// Whether the power timer's confirmation has already been raised for the
    /// instant it is armed for.
    ///
    /// One flag instead of a rule at every place a popup can come down — a
    /// click, `Esc`, a page change — because the answer is the same at all of
    /// them and none of them is about the power timer. Cancel, dismiss and walk
    /// away all mean the same thing: no. Without this the question would be
    /// raised again a second later, and the only way to keep a machine awake
    /// would be to sit and cancel a popup every second until the grace window
    /// ran out. Cleared when the timer is armed or disarmed, so the next
    /// question is a new one.
    power_asked: bool,
}


/// Show the window, creating it the first time. `existing` is the caller's
/// remembered handle; an invalid one means "not up yet".
pub fn ensure(
    instance: HINSTANCE,
    cfg: &Config,
    hint: &TrayModel,
    existing: HWND,
    speed: SpeedTest,
) -> Option<HWND> {
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
        clock: create_font(cfg, 96, CLOCK_EXTRA, true),
        page: OVERVIEW,
        hover: None,
        tracking: false,
        dpi: 96,
        // Owned by the window from here, same as the fonts: `WM_NCDESTROY`
        // deletes it.
        face_brush: unsafe { CreateSolidBrush(bg) },
        instance,
        settings: SettingsForm::from_config(cfg),
        notice: None,
        handed_back: false,
        power_asked: false,
        speed,
        prev_running: false,
        selected_port: None,
        modal: Modal::default(),
        port_rows: Vec::new(),
        port_scroll: 0,
        port_notice: None,
        watch: Stopwatch::default(),
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
        let style = WINDOW_STYLE(WS_OVERLAPPEDWINDOW.0 | WS_CLIPCHILDREN.0);
        let ex_style = WINDOW_EX_STYLE(0);
        // Centred on the size that will actually be created, not on the client
        // size: centring on `START_W`/`START_H` put the window 39 pixels off
        // centre on both axes.
        let (win_w, win_h) = outer_size(style, ex_style, GetDpiForSystem());
        let x = if screen_w > win_w {
            (screen_w - win_w) / 2
        } else {
            CW_USEDEFAULT
        };
        let y = if screen_h > win_h {
            (screen_h - win_h) / 2
        } else {
            CW_USEDEFAULT
        };

        // Built rather than a literal so the caption names the build. The first
        // question about any screenshot of this window is which version drew it,
        // and the answer was previously nowhere on screen. It outlives the call:
        // `CreateWindowExW` copies the string into the window's own storage.
        let caption: Vec<u16> = format!("ArboTray {}", crate::update::current())
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            PCWSTR(caption.as_ptr()),
            // `WS_CLIPCHILDREN` keeps the parent's repaints out of the sidebar,
            // which is what stops the list flickering once per sample.
            style,
            x,
            y,
            win_w,
            win_h,
            None,
            None,
            Some(instance),
            Some(state as *const core::ffi::c_void),
        )
        .ok()?
    };

    Some(hwnd)
}

/// The window size that gives a `START_W`-by-`START_H` client area.
///
/// Every metric in `layout` is a measurement of the *client* area — `START_H` is
/// the room the last settings band needs, counted from the top of the client
/// area — but `CreateWindowExW` takes the size of the whole window. Passing the
/// layout's own numbers straight through opened the window a caption and two
/// borders short of what the layout believed it had, which put the Save row 16
/// pixels under the frame's bottom edge and left nothing that could notice: the
/// test asserts against `START_H` and `START_H` was right.
fn outer_size(style: WINDOW_STYLE, ex_style: WINDOW_EX_STYLE, dpi: u32) -> (i32, i32) {
    let mut r = RECT {
        left: 0,
        top: 0,
        right: scale(START_W, dpi),
        bottom: scale(START_H, dpi),
    };
    // SAFETY: a `RECT` in and out, with no window or handle involved, so there
    // is nothing here to fail beyond a style the API does not recognise — in
    // which case the frame's own size is the whole of the correction and the
    // rectangle comes back unchanged.
    unsafe {
        let _ = AdjustWindowRectExForDpi(&mut r, style, false, ex_style, dpi);
    }
    (r.right - r.left, r.bottom - r.top)
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
///
/// Returns a config to adopt instead of the one the tray is running with, and
impl UiState {
    /// Do what a confirmed popup asked for.
    ///
    /// The one place this window changes something outside itself, and it is
    /// reached only from a confirmation — never from a click, and never from a
    /// sample. Every action reports through `port_notice` afterwards, including
    /// the failures, because "nothing happened" is the one outcome a destructive
    /// button must never give silently.
    fn run_action(&mut self, action: Action) {
        match action {
            Action::StopPort { pid, name } => {
                if crate::telemetry::ports::stop_process(pid) {
                    // Cleared here rather than left to the next sample: the
                    // process is gone, so its row is about to be, and a
                    // selection on a row that is leaving is a highlight on
                    // whatever slides up into its place.
                    if self.selected_port == Some(pid) {
                        self.selected_port = None;
                    }
                    self.port_notice = Some(format!("stopped {name}"));
                } else {
                    // The expected answer on an unelevated process, so it is
                    // phrased as what to do about it rather than as a fault.
                    self.port_notice =
                        Some(format!("could not stop {name} — try running as administrator"));
                }
            }
            Action::PowerTimer { action } => {
                // Disarmed first, and before the call rather than after: a
                // suspend that succeeds never returns, so a clear written
                // afterwards would be a line that only ever ran on the failure
                // path and the timer would fire again the moment the machine
                // woke. The file is the only record, so it goes down first.
                self.cfg.timer.enabled = false;
                self.cfg.timer.armed.clear();
                let written = self.cfg.save();
                self.handed_back = true;
                let done = match action {
                    power::Action::Sleep => power::sleep(),
                    power::Action::Shutdown => power::shutdown(),
                };
                self.notice = Some(match (done, written) {
                    (Ok(()), _) => format!("{} now", action.label().to_lowercase()),
                    // Windows refused it. Said plainly and with the timer left
                    // disarmed, because the alternative — silently re-arming
                    // and trying again a minute later — is a machine that
                    // shuts down an hour after the user cancelled.
                    (Err(e), _) => e,
                });
            }
        }
    }

    /// A Cancel on the power timer: the machine stays up, and so does the
    /// timer's arm come down.
    ///
    /// Disarmed rather than merely unanswered, because a question that has been
    /// answered *no* is answered: leaving it armed would put the same popup back
    /// on screen a second later and the only way to keep working would be to
    /// keep cancelling it. The stored stamp goes with the arm, exactly as
    /// `toggle_arm`'s disarm does — the same state change reached from two
    /// places, so both read the same three fields.
    fn cancel_power_timer(&mut self, hwnd: HWND, action: power::Action) {
        self.cfg.timer.enabled = false;
        self.cfg.timer.armed.clear();
        let written = self.cfg.save();
        self.handed_back = true;
        self.notice = Some(match written {
            Ok(()) => format!("{} called off", action.label().to_lowercase()),
            Err(e) => format!("called off, but could not write the config: {e}"),
        });
        // The page behind the popup is showing a timer that no longer exists:
        // the watch goes with the arm, the button relabels itself to "Arm", and
        // the four fields go back to what is on disk. `set_timer_tick` clears
        // `power_asked` as it goes, so the same instant asked about twice is a
        // new question if it is ever asked again.
        self.refresh_timer_page(hwnd);
    }

    /// Put the Timer page back in step with the config, after something other
    /// than the page itself changed the timer.
    ///
    /// Three calls that always travel together: the watcher follows the arm, the
    /// button names it, and the four fields show what was saved.
    fn refresh_timer_page(&mut self, hwnd: HWND) {
        set_timer_tick(hwnd, self);
        sync_arm_button(hwnd, self);
        sync_timer_fields(hwnd, &self.cfg);
    }

    /// Take the question down without answering it — `Esc`, or a page change.
    ///
    /// The two ways out of the popup that are not a click, and the reason this
    /// is a method rather than the bare `Modal::dismiss` it looks like: a power
    /// question walked away from is a question answered *no*, and leaving the
    /// timer armed would put the same popup back a second later with no way to
    /// refuse it. Every other question is a shrug — the port row it was about
    /// may still be there when the user comes back to it.
    fn dismiss_modal(&mut self, hwnd: HWND) {
        let asked = self.modal.pending_action();
        self.modal.dismiss();
        if let Some(Action::PowerTimer { action }) = asked {
            self.cancel_power_timer(hwnd, action);
        }
    }

    /// The question a Stop click raises, from the current selection.
    ///
    /// `None` when nothing is selected or the row has gone, which is the
    /// button's disabled state — the two agree because both read
    /// `selected_port` rather than either caching its own answer.
    fn stop_question(&self) -> Option<Confirm> {
        let pid = self.selected_port?;
        let entry = self.model.open_ports.iter().find(|p| p.pid == pid)?;
        // What to call it in the question. The row's value is
        // `"svchost.exe  pid 1024"`, and the pid is already in the sentence
        // below, so only the name is taken.
        let name = entry
            .owner
            .split("  pid ")
            .next()
            .unwrap_or(&entry.owner)
            .to_string();
        Some(Confirm {
            title: format!("Stop the process on {}?", entry.port),
            body: format!(
                "This ends {name} (pid {pid}), which is holding {}. Anything unsaved in it is lost.",
                entry.port
            ),
            action: Action::StopPort { pid, name },
        })
    }
}

/// only ever once per edit. The Settings page has to reach a renderer that
/// lives on the tray window, and the telemetry thread cannot do it: it is
/// neither the window's thread nor the edit's author. So the page writes what
/// it can — its own copy — and returns the difference here, on the one call
/// that already runs on the right thread with both windows in hand. See
/// `save_settings`.
pub fn update(hwnd: HWND, model: &TrayModel) -> Option<Config> {
    if hwnd.is_invalid() {
        return None;
    }
    // SAFETY: same thread as the window, so the state pointer cannot race
    // anyone. `IsWindow` covers a handle that has been torn down.
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            return None;
        }
        let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut UiState;
        if state.is_null() {
            return None;
        }
        (*state).model = model.clone();
        // A port the user had selected can close between two samples — by
        // itself, or by the process exiting. The selection is dropped when the
        // pid is no longer in the list at all, rather than left standing: a
        // highlight on a row nobody chose is worse than no highlight, and the
        // Stop button is aimed by the same selection.
        if let Some(pid) = (*state).selected_port {
            if !model.open_ports.iter().any(|p| p.pid == pid) {
                (*state).selected_port = None;
            }
        }
        // The Run button's enablement is the one piece of a page that is not a
        // pixel. It has to be refreshed per tick because the run that re-enables
        // it ends on the telemetry thread, which cannot touch a control — and a
        // menu command or a repaint is not guaranteed to arrive in between.
        if !model.speed_running || !(*state).prev_running {
            set_enabled(hwnd, SET_SPEED, !model.speed_running);
        }
        (*state).prev_running = model.speed_running;
        // The content area only: the sidebar does not change with a sample.
        let _ = InvalidateRect(Some(hwnd), None, false);

        let s = &mut *state;
        // `take`, not a clone-and-clear: an early return between the two would
        // hand the same edit back on every tick, and the tray repaint that
        // follows is what makes a font or colour change visible at all.
        if s.handed_back {
            s.handed_back = false;
            return Some(s.cfg.clone());
        }
        None
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


unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            let create = lparam.0 as *const CREATESTRUCTW;
            if !create.is_null() {
                let state = (*create).lpCreateParams as *mut UiState;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
                // Deliberately no early return. `DefWindowProcW` is the call
                // that copies `lpszName` out of the `CREATESTRUCTW` and into
                // the window, so answering TRUE here left every window this
                // file creates — dashboard and widget alike — captioned with an
                // empty string, which is why the version in the title bar was
                // never once on screen.
            }
        }

        let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut UiState;

        match msg {
            // Before the first paint, so nothing is ever drawn into a window
            // that has no sidebar to lay out. The sidebar needs no child of its
            // own now — `chrome` is here for the frame around the client area.
            WM_CREATE => {
                if !state.is_null() {
                    let s = &mut *state;
                    create_settings(hwnd, s);
                    layout(hwnd, s);
                    chrome(hwnd, s);
                    // A timer armed in an earlier run is already live when the
                    // dashboard is first opened, so the tick has to start here
                    // and not only on an Arm click.
                    set_timer_tick(hwnd, s);
                }
                LRESULT(0)
            }

            WM_SIZE => {
                if !state.is_null() {
                    layout(hwnd, &mut *state);
                }
                LRESULT(0)
            }

            // The Settings page's own controls, dispatched on id alone. The
            // page list used to arrive here too; it is drawn by us now, and
            // picking a page is a click below.
            WM_COMMAND => {
                if !state.is_null() {
                    let s = &mut *state;
                    let id = control_id(wparam);
                    if id == SET_QUOTA_ON {
                        // Disabled rather than silently ignored: with the
                        // switch off the number has no meaning, and leaving it
                        // editable would suggest it does.
                        set_enabled(hwnd, SET_QUOTA, is_checked(hwnd, SET_QUOTA_ON));
                        s.notice = None;
                    } else if id == SET_AUTOSTART {
                        // Written on the click rather than staged for Save,
                        // because it is the one control on the page that does
                        // not describe `config.json`: the registry is the
                        // authority, so there is nothing here to defer.
                        let want = is_checked(hwnd, SET_AUTOSTART);
                        if crate::autostart::set(want) {
                            // Worded to start with `saved` so it paints as a
                            // result rather than as a failure — that prefix is
                            // the whole success/failure signal the notice line
                            // has, see `paint::settings`.
                            s.notice = Some(if want {
                                "saved: starts with Windows".into()
                            } else {
                                "saved: no longer starts with Windows".into()
                            });
                        } else {
                            // Put the tick back. A checkbox left showing a
                            // state the registry did not accept is a lie about
                            // what will happen at logon.
                            set_check(hwnd, SET_AUTOSTART, !want);
                            s.notice = Some("could not change the startup entry".into());
                        }
                        repaint_after_settings(hwnd, s);
                    } else if let Some(box_id) = pick_target(id) {
                        // Modal to this window, so the page cannot be edited
                        // while it is up. A dismissal writes nothing: `Cancel`
                        // must not be a way to blank a colour field, and the
                        // dialog is asked for on the strength of a colour the
                        // user already has.
                        if choose_color(hwnd, box_id) {
                            // The notice described the last save, and this makes
                            // the page differ from disk — the same reason the
                            // quota switch clears it.
                            s.notice = None;
                            // A background picked by hand moves the appearance
                            // row too: the row's word and glyph describe which
                            // way the page goes, and the box just written is the
                            // only thing that knows.
                            s.settings = read_form(hwnd);
                            write_theme_button(hwnd, &s.settings.background());
                            let _ = InvalidateRect(Some(hwnd), None, false);
                        }
                    } else if id == SET_THEME {
                        // Types into the two colour fields rather than applying
                        // anything: the page stays staged behind Save, so this
                        // is undoable by pressing Reload, and the window keeps
                        // the colours it has until the user commits.
                        let caption = apply_preset(hwnd, s);
                        s.notice = Some(format!("{caption} colours — press Save to apply"));
                        repaint_after_settings(hwnd, s);
                    } else if id == SET_SAVE {
                        let notice = save_settings(hwnd, s);
                        s.notice = Some(notice);
                        // The notice is painted by us, so nothing repaints it
                        // on its own — and a save that changed the theme has
                        // to show its own result here too.
                        repaint_after_settings(hwnd, s);
                    } else if id == SET_STOP {
                        // Nothing happens here: the click only raises the
                        // question. The kill is done by `run_action` once the
                        // popup has been answered, which is what keeps a
                        // destructive call off the click path entirely.
                        if let Some(confirm) = s.stop_question() {
                            s.modal.open(confirm);
                            // The notice described the selection this question
                            // is about, so it is stale the moment it is asked.
                            s.port_notice = None;
                            let _ = InvalidateRect(Some(hwnd), None, false);
                        }
                    } else if id == SET_SPEED {
                        // Refused rather than restarted when one is already in
                        // flight: a test cancelled halfway reports half a link,
                        // and the phase row — not the button — is the feedback.
                        let _ = s.speed.start();
                        // The run itself is reported by the telemetry thread a
                        // tick later, so nothing is read back here. Repaint so
                        // the button greys out on the same click rather than on
                        // whatever the next sample happens to be.
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    } else if id == SET_WATCH {
                        // One button for both directions. The clock is toggled
                        // first and the caption is read back off it, so the two
                        // cannot disagree.
                        let running = s.watch.toggle();
                        set_watch_timer(hwnd, running);
                        sync_watch_button(hwnd, s);
                        // The page is painted by us, so the reading the clock
                        // stopped on — and the hint that goes away with it —
                        // need a repaint that no sample is going to bring.
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    } else if id == SET_WATCH_RESET {
                        s.watch.reset();
                        // Reset stops as well as zeroes, so this is also the
                        // path that stops the clock beside putting the caption
                        // back to "Start".
                        set_watch_timer(hwnd, false);
                        sync_watch_button(hwnd, s);
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    } else if id == SET_TIMER_MODE {
                        // One button for both directions, like the Stopwatch's.
                        // The caption is the selection, so it is read back from
                        // the same `power` enum the engine will parse rather
                        // than from a table here that could drift from it.
                        let form = read_timer(hwnd);
                        let mode = power::Mode::parse(&form.mode).unwrap_or(power::Mode::AtTime);
                        let mut form = form;
                        form.mode = mode.flipped().key().to_string();
                        set_text(hwnd, SET_TIMER_MODE, mode.flipped().label());
                        // The other field greys out on the same click: which of
                        // the two is live is a property of the mode, not of what
                        // the user has typed into either.
                        match mode.flipped() {
                            power::Mode::AtTime => {
                                set_enabled(hwnd, SET_TIMER_AT, true);
                                set_enabled(hwnd, SET_TIMER_WAIT, false);
                            }
                            power::Mode::Countdown => {
                                set_enabled(hwnd, SET_TIMER_AT, false);
                                set_enabled(hwnd, SET_TIMER_WAIT, true);
                            }
                        }
                        s.notice = None;
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    } else if id == SET_TIMER_ACTION {
                        let form = read_timer(hwnd);
                        let action = power::Action::parse(&form.action).unwrap_or(power::Action::Sleep);
                        set_text(hwnd, SET_TIMER_ACTION, action.flipped().label());
                        s.notice = None;
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    } else if id == SET_TIMER_ARM {
                        let notice = toggle_arm(hwnd, s);
                        s.notice = Some(notice);
                        // The arm button's verb and the page's status line are
                        // both ours to draw, and neither arrives with a sample.
                        sync_arm_button(hwnd, s);
                        sync_timer_fields(hwnd, &s.cfg);
                        set_timer_tick(hwnd, s);
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    } else if id == SET_RESET {
                        // Back to what is on disk — not to the built-in
                        // defaults, and not to whatever is running. The file is
                        // the thing the user can see and edit; a reset button
                        // that invented a third state would be a trap.
                        let loaded = Config::load();
                        s.settings = SettingsForm::from_config(&loaded);
                        s.cfg = loaded.clone();
                        write_form(hwnd, &loaded);
                        s.notice = Some(format!("reloaded {}", Config::path().display()));
                        repaint_after_settings(hwnd, s);
                    }
                }
                LRESULT(0)
            }

            // The settings controls are system controls and would paint
            // themselves in the system's grey-on-white. Hand them the theme's
            // own face and text instead. `WM_CTLCOLORBTN` is only honoured by
            // checkboxes and not by push buttons — Windows draws a push button
            // from its own parts and ignores the brush, which is why Save keeps
            // its native look and the checkboxes do not.
            WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORBTN => {
                if !state.is_null() {
                    let s = &*state;
                    let dc = windows::Win32::Graphics::Gdi::HDC(wparam.0 as *mut core::ffi::c_void);
                    let _ = SetBkColor(dc, background(&s.cfg));
                    let _ = SetTextColor(dc, foreground(&s.cfg));
                    return LRESULT(s.face_brush.0 as isize);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }

            // The pointer moved: find out which entry, if any, is under it, and
            // repaint only when that changed. The sidebar is drawn, so hover is
            // a pixel we have to maintain rather than a state the system keeps.
            WM_MOUSEMOVE => {
                if !state.is_null() {
                    let s = &mut *state;
                    // Signed halves, not the raw `lparam`: a mouse above or to
                    // the left of the window reports a negative coordinate, and
                    // reading them as unsigned would put the pointer thousands
                    // of pixels inside the sidebar.
                    let x = (lparam.0 & 0xFFFF) as u16 as i16 as i32;
                    let y = ((lparam.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
                    // The popup is modal, so nothing under it hovers: a
                    // sidebar pill lighting up behind a dimmed scrim reads as a
                    // window that cannot decide whether it is blocked.
                    let hover = if s.modal.is_open() {
                        None
                    } else {
                        sidebar::hit_test(s, x, y)
                    };
                    if hover != s.hover {
                        s.hover = hover;
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                    // Armed once per visit. `TrackMouseEvent` fires a single
                    // `WM_MOUSELEAVE` and then stops, so this re-arms it while
                    // the pointer is here — but only when it is not already
                    // pending, which is what keeps it off the per-pixel path.
                    if !s.tracking {
                        let mut track = TRACKMOUSEEVENT {
                            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        };
                        if TrackMouseEvent(&mut track).is_ok() {
                            s.tracking = true;
                        }
                    }
                }
                LRESULT(0)
            }

            // The pointer left. One of these per visit, so the flag is cleared
            // here rather than on every move.
            WM_MOUSELEAVE => {
                if !state.is_null() {
                    let s = &mut *state;
                    s.tracking = false;
                    if s.hover.take().is_some() {
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                }
                LRESULT(0)
            }

            // A click anywhere in the sidebar. Off an entry it does nothing at
            // all — not even clear the selection — because the list is navigation
            // and there is nowhere to navigate away from.
            WM_LBUTTONDOWN => {
                if !state.is_null() {
                    let s = &mut *state;
                    let x = (lparam.0 & 0xFFFF) as u16 as i16 as i32;
                    let y = ((lparam.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
                    let mut client = RECT::default();
                    let _ = GetClientRect(hwnd, &mut client);

                    // The popup takes the click first and swallows it whether or
                    // not it landed on a button. Nothing behind it is reachable
                    // while it is up — that is what makes it a modal rather than
                    // a floating panel.
                    if s.modal.is_open() {
                        // Read before the click, because the click takes the
                        // question down as it answers it: a Cancel has to know
                        // *what* it is a no to. Saying no to the power timer
                        // disarms it, while a click that merely missed a port
                        // button leaves that question as it was.
                        let asked = s.modal.pending_action();
                        let outcome = s.modal.click(&client, s.dpi, x, y);
                        match outcome {
                            Some(Outcome::Confirmed(action)) => {
                                let was_power =
                                    matches!(action, Action::PowerTimer { .. });
                                s.run_action(action);
                                // Only on the failure path — a suspend that
                                // succeeds never returns — but that is exactly
                                // the path the page has to be right about, since
                                // it is the one the user is still sitting in
                                // front of.
                                if was_power {
                                    s.refresh_timer_page(hwnd);
                                }
                            }
                            Some(Outcome::Cancelled) => {
                                if let Some(Action::PowerTimer { action }) = asked {
                                    s.cancel_power_timer(hwnd, action);
                                }
                            }
                            // A click that landed on nothing: the question went
                            // back exactly as it was and the frame only loses
                            // the scrim for as long as it takes to repaint.
                            None => {}
                        }
                        let _ = InvalidateRect(Some(hwnd), None, false);
                        return LRESULT(0);
                    }

                    if let Some(page) = sidebar::hit_test(s, x, y) {
                        // A question about a row on the page being left is a
                        // question about nothing.
                        s.dismiss_modal(hwnd);
                        // `select` shows and hides the Settings controls and
                        // invalidates, so the repaint is not repeated here.
                        sidebar::select(hwnd, s, page);
                        return LRESULT(0);
                    }

                    // The Ports page's list: a click selects the row's process
                    // as the thing the Stop button is aimed at. Anywhere else on
                    // the page clears the selection, so there is a way to take
                    // the aim back without leaving the page.
                    if s.page == PORTS {
                        // The pid comes out of the band itself: the list is
                        // scrolled, so a band's index here is where it sits on
                        // screen, not where the port sits in the model.
                        let hit = s
                            .port_rows
                            .iter()
                            .find(|(r, _)| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
                            .map(|(_, pid)| *pid);
                        if hit != s.selected_port {
                            s.selected_port = hit;
                            // The notice described the *previous* selection, so
                            // it goes with it.
                            s.port_notice = None;
                            let _ = InvalidateRect(Some(hwnd), None, false);
                        }
                    }
                }
                LRESULT(0)
            }

            // The wheel, over the Ports page's list. The one list in this window
            // that can be taller than its page: a dev machine holds more
            // listeners than fit between the counters and the Stop button, and
            // the port the reader came for is as likely to be below the fold as
            // above it.
            //
            // Handled on the *window* rather than on a child, because the list
            // is painted rather than a control — there is no scrollable window
            // to receive this, so the frame has to be the one that does. Nothing
            // else in the window scrolls, so the wheel over any other page does
            // nothing at all rather than being forwarded.
            WM_MOUSEWHEEL => {
                if !state.is_null() {
                    let s = &mut *state;
                    // A modal owns the window's input while it is up, and the
                    // wheel is input like any other: a page scrolling under a
                    // question about one of its rows is the same failure as a
                    // click reaching behind it.
                    if s.modal.is_open() || s.page != PORTS {
                        return LRESULT(0);
                    }
                    // Signed, and out of the high word: a notch toward the user
                    // is negative, so a positive delta scrolls the list back.
                    let delta = (((wparam.0 >> 16) & 0xFFFF) as u16 as i16 as i32) / WHEEL_DELTA;
                    if delta != 0 {
                        s.port_scroll = scroll_by(s.port_scroll, delta);
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                }
                LRESULT(0)
            }

            // The listbox used to move between pages on the arrow keys by
            // itself. It is drawn by us now, so the keys are ours too: without
            // this the sidebar becomes mouse-only and the window loses the
            // keyboard navigation it has always had.
            WM_KEYDOWN => {
                if !state.is_null() {
                    let s = &mut *state;
                    // Escape answers the popup and nothing else. Checked first,
                    // and before the arrow keys: a modal that lets the page
                    // change under it is a modal the user can lose.
                    if s.modal.is_open() {
                        if wparam.0 as u16 == VK_ESCAPE.0 {
                            s.dismiss_modal(hwnd);
                            let _ = InvalidateRect(Some(hwnd), None, false);
                        }
                        return LRESULT(0);
                    }
                    let last = PAGES.len() - 1;
                    let page = match wparam.0 {
                        VK_UP => s.page.saturating_sub(1),
                        VK_DOWN => (s.page + 1).min(last),
                        _ => {
                            return DefWindowProcW(hwnd, msg, wparam, lparam);
                        }
                    };
                    sidebar::select(hwnd, s, page);
                }
                LRESULT(0)
            }

            WM_PAINT => {
                if state.is_null() {
                    return DefWindowProcW(hwnd, msg, wparam, lparam);
                }
                let mut ps = PAINTSTRUCT::default();
                let _ = BeginPaint(hwnd, &mut ps);
                paint(hwnd, &mut *state);
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }

            // The Stopwatch page's clock. Armed by a Start and killed by a Stop
            // or a Reset, so it cannot arrive while the clock is paused — which
            // is what keeps the page static when it should be, rather than
            // repainting four identical frames a second.
            WM_TIMER if wparam.0 == TIMER_WATCH => {
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }

            // The power timer's watch. Armed only while a timer is, and the
            // only timer here whose tick decides anything: it can raise the
            // confirmation popup, and without an answer it disarms rather than
            // fires. Everything the countdown on the page shows is redrawn by
            // the invalidate either way.
            WM_TIMER if wparam.0 == TIMER_POWER => {
                if !state.is_null() {
                    poll_power_timer(hwnd, &mut *state);
                }
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }

            // Painted edge to edge below; letting the default erase first is
            // exactly the flicker this avoids.
            WM_ERASEBKGND => LRESULT(1),

            WM_DPICHANGED => {
                if !state.is_null() {
                    let dpi = (wparam.0 & 0xFFFF) as u32;
                    let s = &mut *state;
                    s.dpi = if dpi == 0 { 96 } else { dpi };
                    // Every face and both brushes are built from the DPI and
                    // the theme, and both moved. The controls are given the new
                    // font by `layout`.
                    rebuild_fonts(s);
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
                    let _ = DeleteObject(HGDIOBJ(s.clock.0));
                    let _ = DeleteObject(HGDIOBJ(s.face_brush.0));
                }
                LRESULT(0)
            }

            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}


/// The window timer that keeps the Stopwatch page's clock moving, and how often
/// it fires.
///
/// A hundred milliseconds, because the reading carries tenths while it runs —
/// at a second the seconds digit would jump with the tenths frozen between
/// them, which reads as a stutter rather than as a clock. The tick only moves
/// one number, so the cost is a repaint of a small window.
///
/// The id is arbitrary but must be unique among this window's timers, of which
/// there are two — the other is `TIMER_POWER` below.
const TIMER_WATCH: usize = 1;
const WATCH_TICK_MS: u32 = 100;

/// Arm or disarm the Stopwatch page's repaint timer.
///
/// A win32 timer owned by this window. The clock
/// cannot ride the tray's own tick: that is a second at its shortest, and a
/// user who set `interval_ms` to a minute would be watching a stopwatch that
/// advanced once a minute — which is not a slower stopwatch, it is a broken one.
///
/// It is armed on start and killed on stop rather than left running, so a
/// paused clock costs nothing, and `SetTimer` with an id that is already armed
/// only resets the interval — calling this twice is not two timers.
fn set_watch_timer(hwnd: HWND, running: bool) {
    // SAFETY: our own window, and an id that belongs to no other timer here.
    unsafe {
        if running {
            SetTimer(Some(hwnd), TIMER_WATCH, WATCH_TICK_MS, None);
        } else {
            let _ = KillTimer(Some(hwnd), TIMER_WATCH);
        }
    }
}

/// The window timer that watches the sleep / shut-down timer, and how often it
/// looks.
///
/// A second, for a decision graded in minutes: the confirmation is raised a
/// minute out and the fire is acted on within a second of coming due, so five
/// seconds would be plenty. A second is chosen anyway because it keeps the
/// countdown on the page moving, and the repaint is one invalidate.
///
/// Id 2, beside the Stopwatch's 1 — the numbers must be unique per window and
/// nothing else here owns a timer.
const TIMER_POWER: usize = 2;
const POWER_TICK_MS: u32 = 1000;

/// Arm or disarm the power timer's watch.
///
/// Armed by an Arm click and killed by a Disarm, so a machine with no timer set
/// pays nothing for the feature. The state it watches lives in the config rather
/// than in this window, so this is a pure function of that config and is safe to
/// call on every path that changes it — `SetTimer` on an armed id only resets
/// the interval rather than adding a second timer.
fn set_timer_tick(hwnd: HWND, state: &mut UiState) {
    // Whatever happens next is a different question from the one on screen —
    // a re-arm names a new instant, and a disarm means there is nothing left to
    // ask about. Either way the flag goes with it.
    state.power_asked = false;
    // SAFETY: our own window, and an id no other timer here claims.
    unsafe {
        if state.cfg.timer.enabled {
            SetTimer(Some(hwnd), TIMER_POWER, POWER_TICK_MS, None);
        } else {
            let _ = KillTimer(Some(hwnd), TIMER_POWER);
        }
    }
}

/// One look at the power timer: raise the question, or answer it.
///
/// The engine owns every decision here — `power::phase` says which of the four
/// states the instant is in, and the grace window inside it is what keeps a
/// laptop that was suspended before midnight from being put straight back to
/// sleep the moment it wakes with a stale target behind it.
///
/// Called from the tick. It takes `&mut UiState` because it raises the modal,
/// and it never fires anything itself: `run_action` is the only caller of
/// `power::sleep`/`power::shutdown`, and it runs only once the popup has been
/// answered.
fn poll_power_timer(hwnd: HWND, state: &mut UiState) {
    if !state.cfg.timer.enabled || state.modal.is_open() || state.power_asked {
        return;
    }
    let now = power::now();
    let Some(when) = power::fire_at(&state.cfg.timer, &now) else {
        return;
    };
    match power::phase(&when, &now) {
        // The minute before: ask, once. The countdown in the body is what makes
        // a mistyped time harmless — the question is on screen with a Cancel
        // beside it for a full minute before anything happens.
        power::Phase::Warning => {
            state.power_asked = true;
            state.modal.open(power_question(state, &when));
            // SAFETY: our own window; the popup is painted, not a child.
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
        // Past its minute but still inside the grace window, and the question
        // was answered with a Cancel — or was never raised, because the machine
        // was asleep through it. Treated as spent rather than re-asked: a timer
        // that comes back from a suspend and immediately shuts the machine down
        // is worse than one that misses its night.
        power::Phase::Due | power::Phase::Stale => {
            state.cfg.timer.enabled = false;
            state.cfg.timer.armed.clear();
            let _ = state.cfg.save();
            state.handed_back = true;
            state.notice = Some(format!(
                "timer passed at {} without confirming — disarmed",
                power::format_stamp(&when)
            ));
            set_timer_tick(hwnd, state);
            sync_arm_button(hwnd, state);
            // SAFETY: as above.
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
        }
        power::Phase::Waiting => {}
    }
}

/// The question the power timer raises a minute before it fires.
///
/// The countdown is recomputed from the same instant the engine will fire on,
/// so the number in the sentence and the number the watcher is counting are one
/// number.
fn power_question(state: &UiState, when: &SYSTEMTIME) -> Confirm {
    let action = power::action_of(&state.cfg.timer);
    let secs = power::seconds_until(when, &power::now());
    Confirm {
        title: format!("{} in {}?", action.label(), power::format_countdown(secs)),
        body: format!(
            "The timer is set for {}. Cancel to stop it, or let it run and the machine will {}.",
            power::format_stamp(when),
            if action == power::Action::Shutdown {
                "shut down"
            } else {
                "sleep"
            }
        ),
        action: Action::PowerTimer { action },
    }
}

/// The low half of a `WPARAM`, which is where `WM_COMMAND` packs the child id.
/// `LOWORD` is not in the bindings and the mask is the whole of it.
///
/// The high half — the notification code — is deliberately not unpacked: the
/// only control that ever sent one worth filtering on was the page list, and a
/// click on the sidebar is a mouse message now.
pub(crate) fn low_word(value: usize) -> u16 {
    (value & 0xFFFF) as u16
}

/// A scroll offset moved by `notches`, and never below zero.
///
/// Unsigned because an offset is a count of rows from the top, and the negative
/// direction is what a `usize` would turn into four billion. The upper bound is
/// not here: only the painter knows how much room the list got, so it clamps
/// against that rather than against a guess made here. Scrolling past the end is
/// therefore possible by exactly this much and is corrected on the next paint —
/// which is also the case where the window was resized between the two.
pub(crate) fn scroll_by(offset: usize, notches: i32) -> usize {
    if notches >= 0 {
        offset.saturating_add(notches as usize)
    } else {
        offset.saturating_sub(notches.unsigned_abs() as usize)
    }
}

/// Make the title bar and the frame match the theme.
///
/// The client area is painted by us and everything around it is drawn by the
/// shell, so without this a dark theme wears a white caption and the mismatch
/// is the first thing anyone notices. Three attributes, each independently
/// optional:
///
/// * `DWMWA_USE_IMMERSIVE_DARK_MODE` — the caption and the system buttons.
/// * `DWMWA_WINDOW_CORNER_PREFERENCE` — rounded corners. The one piece of
///   Windows' own chrome that already looks like the rest of the window.
/// * `DWMWA_BORDER_COLOR` — the frame, in the palette's divider colour, so the
///   outline is the same step off the surface that separates the sidebar.
///
/// Every call is tolerant of failure by construction: all three attributes are
/// Windows 11 additions, and on Windows 10 each one returns an error, having
/// changed nothing. A window with a square border and a system-coloured frame
/// is the Windows 10 result and is perfectly usable, which is why this is not
/// worth a version check.
fn chrome(hwnd: HWND, state: &UiState) {
    let pal = crate::ui::design::palette(&state.cfg);
    let dark = i32::from(crate::ui::design::is_dark(&state.cfg));
    let corner = DWMWCP_ROUND;
    // SAFETY: three by-value attributes handed to the compositor for our own
    // window. Each pointer is to a local that outlives the call, and each size
    // is that local's own.
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const i32 as *const core::ffi::c_void,
            size_of::<i32>() as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const _ as *const core::ffi::c_void,
            size_of_val(&corner) as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &pal.border as *const _ as *const core::ffi::c_void,
            size_of_val(&pal.border) as u32,
        );
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::QUOTA_MIN_GB;
    use windows::Win32::Foundation::COLORREF;

    fn full() -> TrayModel {
        TrayModel {
            down_text: "1.4M/s".into(),
            up_text: "0.2M/s".into(),
            latency_text: "8ms".into(),
            cpu_text: "12%".into(),
            ram_text: "44%".into(),
            gateway_text: "192.168.1.1".into(),
            internet_text: "14ms".into(),
            loss_text: "0%".into(),
            wifi_text: "5G 78%".into(),
            wifi_name: Some("HomeNet".into()),
            adapter_text: "Wi-Fi".into(),
            ip_text: "192.168.1.10".into(),
            dns_text: "192.168.1.1, 8.8.8.8".into(),
            usage_text: "1.4G".into(),
            quota_alert: false,
            month_text: "41.2G".into(),
            month_partial: false,
            usage_days: vec![("09-14".into(), 1000), ("09-15".into(), 2000)],
            history: vec![1, 2, 3],
            // The System page's detail, filled so the page's own tests see a
            // machine that answered everything rather than one that answered
            // nothing — a blank field here would make a row-ordering assertion
            // pass for the wrong reason.
            computer_text: "STUDIO-PC".into(),
            windows_text: "Windows 11 Pro 24H2 (build 26100.2033)".into(),
            cpu_name_text: "AMD Ryzen 5 7600 6-Core Processor".into(),
            cores_text: "6 cores / 12 threads".into(),
            gpu_text: "AMD Radeon RX 6600".into(),
            battery_text: "88%".into(),
            power_text: "Plugged in".into(),
            uptime_text: "3d 4h".into(),
            // Two volumes, so the page's own tests see the multi-drive path
            // rather than the one-drive case that would pass either way.
            disks: vec![
                ("C:".into(), "210G free of 931G".into()),
                ("D:".into(), "1.2T free of 1.8T".into()),
            ],
            // The Ports page's detail, likewise filled: the open-port list is
            // painted outside `page_rows` and has its own empty state, so the
            // tests that walk a page need the non-empty branch.
            listeners_text: "24".into(),
            established_text: "87".into(),
            udp_text: "31".into(),
            port_owners_text: "42".into(),
            open_ports: vec![
                OpenPort {
                    port: ":445".into(),
                    owner: "System  pid 4".into(),
                    pid: 4,
                },
                OpenPort {
                    port: ":5040".into(),
                    owner: "svchost.exe  pid 1024".into(),
                    pid: 1024,
                },
            ],
            // The Speed Test page's, with a finished run rather than a live one:
            // a running test is the state the page has one row more of, and the
            // ordering tests are about the rows that are always there.
            speed_phase_text: "Done".into(),
            speed_percent: 100,
            speed_running: false,
            speed_down_text: "94.2M/s".into(),
            speed_up_text: "11.8M/s".into(),
            speed_latency_text: "14ms".into(),
            speed_error_text: String::new(),
            speed_when_text: "14:32:07".into(),
            speed_history: vec![("14:32:07".into(), "94.2M/s / 11.8M/s".into())],
            // The build line, filled so the System page's row ordering is
            // exercised with its last row present rather than absent.
            version_text: "0.7.1 \u{2192} 0.8.0".into(),
        }
    }

    fn labels(page: usize, model: &TrayModel) -> Vec<&'static str> {
        page_rows(page, model).into_iter().map(|(l, _)| l).collect()
    }

    #[test]
    fn every_page_is_reachable_from_the_sidebar() {
        // The sidebar is built from `PAGES` and indexed by position, so a page
        // whose rows fall through to `_` would be silently invisible.
        for (i, name) in PAGES.iter().enumerate() {
            assert!(!name.is_empty(), "page {i} has no label to click");
        }
        assert_eq!(PAGES.first(), Some(&"Overview"), "the first page is the one shown");
        assert!(PAGES.len() > 1, "a one-page sidebar is not a sidebar");
        // Settings has to be last: `OVERVIEW`..`DATA` are positional, so a page
        // inserted before them renumbers every page after it.
        assert_eq!(PAGES.last(), Some(&"Settings"));
        assert_eq!(SETTINGS, PAGES.len() - 1);
        for (i, name) in ["Overview", "Network", "System", "Data"].iter().enumerate() {
            assert_eq!(&PAGES[i], name, "page {i} moved");
        }
    }

    #[test]
    fn the_settings_page_has_no_metric_rows() {
        // Without its own arm in `page_rows` it would fall through to the
        // overview and paint traffic figures between its own fields.
        assert!(page_rows(SETTINGS, &full()).is_empty());
        assert!(!page_shows_graph(SETTINGS), "settings are not a traffic story");
    }

    #[test]
    fn every_settings_caption_has_a_row() {
        // The painter zips this array against row indices. A short array would
        // silently drop the last caption — which is the one that names the
        // button that does the work.
        assert_eq!(SET_ROW_LABELS.len(), SET_ROW_COUNT);
        assert_eq!(SET_ROW_LABELS[ROW_SAVE], "Write config.json");
        // The tile rows carry the checkboxes' own labels, so they are blank.
        for row in 0..ROW_REFRESH {
            assert!(SET_ROW_LABELS[row].is_empty(), "row {row} has a caption");
        }
    }

    #[test]
    fn every_field_control_is_laid_out_exactly_once() {
        // `FIELD_ROWS` drives both the layout and the captions, so a control
        // missing from it is created and never positioned — an invisible
        // field — and one listed twice is moved to whichever row won.
        let mut seen: Vec<i32> = Vec::new();
        for (id, row) in FIELD_ROWS {
            assert!(!seen.contains(&id), "control {id} laid out twice");
            assert!(row < SET_ROW_COUNT, "control {id} is off the page");
            seen.push(id);
        }
        assert_eq!(seen.len(), FIELD_ROWS.len());

        // Nothing may collide with a tile checkbox. The sidebar needs no id of
        // its own: it is painted, so nothing arrives for it through `WM_COMMAND`.
        for id in &seen {
            assert!(!TILE_IDS.contains(id), "control {id} collides with a tile");
        }
        // The two Save/Reload buttons are both on the last row.
        assert!(seen.contains(&SET_SAVE));
        assert!(seen.contains(&SET_RESET));
    }

    #[test]
    fn the_tile_ids_are_contiguous_and_theirs_alone() {
        // The grid is built by iterating these in order, so a duplicate would
        // put two checkboxes on one config field.
        for (i, id) in TILE_IDS.iter().enumerate() {
            assert_eq!(*id, SET_ID_BASE + i as i32);
        }
        assert_eq!(TILE_IDS.len(), TILE_LABELS.len());
        // No field id may be one of the tile ids.
        for (id, _) in FIELD_ROWS {
            for tile in TILE_IDS {
                assert_ne!(id, tile, "field {id} shares an id with a tile");
            }
        }
    }

    #[test]
    fn the_tiles_pack_into_four_rows_of_two() {
        // Eight tiles in two columns is four rows only if the packing agrees,
        // and `create_settings` gives `TILE_LABELS[i]` the id at slot `i`.
        let slots: Vec<(usize, usize)> = (0..TILE_IDS.len()).map(tile_slot).collect();
        assert_eq!(slots[0], (0, 0));
        assert_eq!(slots[1], (0, 1));
        assert_eq!(slots[2], (1, 0));
        assert_eq!(slots[TILE_IDS.len() - 1], (3, 1));
        // No two tiles may land on the same slot.
        let mut unique = slots.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), slots.len());
        // And the grid must end before the first labelled field starts.
        let last_row = slots.iter().map(|(r, _)| *r).max().unwrap();
        assert!(last_row < ROW_REFRESH, "the grid overlaps the fields below it");
    }

    #[test]
    fn the_two_tile_columns_do_not_overlap() {
        let (left0, right0) = tile_column(0, 200, 700, 10);
        let (left1, right1) = tile_column(1, 200, 700, 10);
        assert_eq!(left0, 200, "the first column starts at the field edge");
        assert!(right0 < left1, "columns overlap: {right0} >= {left1}");
        assert_eq!(right1, 700, "the last column ends at the field edge");
        // Equal widths, or the two columns look like a mistake.
        assert_eq!(right0 - left0, right1 - left1);
    }

    #[test]
    fn the_tiles_map_to_the_show_flags_and_back() {
        // `tile_flags` and `apply_tiles` are the only place the eight `Show`
        // fields are named as a list, so a round trip is what proves the page
        // shows and writes the same field.
        let mut show = crate::config::Show::default();
        // Flip every one, so a field wired to the wrong slot shows up.
        let flipped: [bool; 8] = std::array::from_fn(|i| i % 2 == 0);
        apply_tiles(&mut show, &flipped);
        assert_eq!(tile_flags(&show), flipped);

        // The stock tile set, in `TILE_LABELS` order. `wifi` is the one left
        // off — a machine without a wireless card should not open with a dead
        // tile — and `usage` is on because the counter exists to be visible
        // without opening the dashboard.
        let back = crate::config::Show::default();
        assert_eq!(tile_flags(&back), [true, true, true, true, true, false, true, true]);
    }

    #[test]
    fn the_scroll_offset_never_goes_negative() {
        // The whole reason the offset is a `usize` moved by a signed amount
        // rather than an `i32`: scrolling up at the top of the list must stop,
        // not wrap to four billion and draw a blank page.
        assert_eq!(scroll_by(0, -1), 0);
        assert_eq!(scroll_by(0, -100), 0);
        assert_eq!(scroll_by(5, -3), 2);
        assert_eq!(scroll_by(5, -50), 0);
        assert_eq!(scroll_by(5, 3), 8);
        // A wheel flick reports several notches in one message.
        assert_eq!(scroll_by(0, 4), 4);
        // The upper bound is the painter's, since only it knows the room.
        assert_eq!(scroll_by(usize::MAX, 1), usize::MAX);
    }

    #[test]
    fn a_port_row_band_carries_the_pid_it_was_drawn_from() {
        // A hit test that resolved a band by its *index* would be right only
        // while the list was scrolled to the top. This is the property that
        // replaced it: the band knows its own port, so the index is irrelevant.
        let rows: Vec<(RECT, u32)> = (0..3)
            .map(|i| {
                (
                    RECT {
                        left: 0,
                        top: 30 * i,
                        right: 200,
                        bottom: 30 * (i + 1),
                    },
                    100 + i as u32,
                )
            })
            .collect();
        let hit = |x: i32, y: i32| {
            rows.iter()
                .find(|(r, _)| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
                .map(|(_, pid)| *pid)
        };
        assert_eq!(hit(10, 0), Some(100));
        assert_eq!(hit(10, 75), Some(102));
        // Below the last row, and above the first: nothing, so a click on the
        // caption or the notice clears the selection rather than grabbing a
        // neighbouring port through a rounding error.
        assert_eq!(hit(10, 90), None);
        assert_eq!(hit(10, -1), None);
    }

    #[test]
    fn a_config_round_trips_through_the_form() {
        // Load the page, save it without touching anything: the file must come
        // back byte-identical. This is the property that makes the page safe to
        // open and close.
        let cfg = Config::default();
        let form = SettingsForm::from_config(&cfg);
        let back = form.into_config(&cfg).expect("an untouched form must be valid");
        assert_eq!(back.to_json(), cfg.to_json());
    }

    #[test]
    fn a_colour_survives_a_round_trip_through_the_picker() {
        // `format_color` is the inverse of `render::parse_color`, and the two
        // are written out separately because one is the config's spelling and
        // the other is GDI's — so a wrong shift in either shows up here as a
        // colour that comes back as itself and still looks wrong on screen.
        for text in ["#FF8000", "#000000", "#FFFFFF", "#123456", "#0A0B0C"] {
            let parsed = crate::taskbar::render::parse_color(text).expect(text);
            assert_eq!(super::settings::format_color(parsed), text, "{text}");
        }
        // And the case that would be invisible in a round trip: channel order.
        // `#FF8000` is orange, and a swapped one is blue.
        let orange = crate::taskbar::render::parse_color("#FF8000").unwrap();
        assert_eq!(orange.0 & 0xFF, 0xFF, "red must land in the low byte");
        assert_eq!((orange.0 >> 16) & 0xFF, 0x00, "blue must land in the high byte");
    }

    #[test]
    fn every_pick_button_edits_a_colour_box_that_is_on_the_page() {
        // A button wired to the wrong box, or to an id no control was created
        // with, is a dialog that opens and then does nothing — which reads as a
        // broken button rather than as a wiring mistake.
        let mut boxes: Vec<i32> = Vec::new();
        for (button, box_id) in PICK_TARGETS {
            assert_eq!(pick_target(button), Some(box_id));
            assert!(
                FIELD_ROWS.iter().any(|(id, _)| *id == box_id),
                "the box {box_id} behind button {button} is never laid out"
            );
            assert!(
                FIELD_ROWS.iter().any(|(id, _)| *id == button),
                "button {button} is never laid out"
            );
            assert!(pick_button(button), "button {button} is not laid out as one");
            boxes.push(box_id);
        }
        // Three buttons, three different colours.
        boxes.sort_unstable();
        boxes.dedup();
        assert_eq!(boxes.len(), PICK_TARGETS.len());
        // A control that is not a pick button must not claim to be one, or the
        // layout gives a field a button's width.
        assert!(!pick_button(SET_SAVE));
        assert!(!pick_button(SET_BG));
        assert_eq!(pick_target(SET_SAVE), None);
    }

    #[test]
    fn the_form_keeps_a_plan_of_zero_switched_off() {
        // `quota_gb == 0.0` is the config's own "no plan", so the page has to
        // show it as the switch being off and write it back as zero — not as
        // the bound it displays in the box.
        let cfg = Config::default();
        assert_eq!(cfg.quota_gb, 0.0);
        let form = SettingsForm::from_config(&cfg);
        assert!(!form.quota_on);
        assert_eq!(form.into_config(&cfg).unwrap().quota_gb, 0.0);

        // With a real plan the switch is on and the number survives.
        let planned = Config {
            quota_gb: 250.0,
            ..Default::default()
        };
        let form = SettingsForm::from_config(&planned);
        assert!(form.quota_on);
        assert_eq!(form.quota, 250.0);
        assert_eq!(form.into_config(&planned).unwrap().quota_gb, 250.0);
    }

    #[test]
    fn an_impossible_font_size_is_refused_not_rounded() {
        // Rounding would let a typo become a number the user never typed.
        let cfg = Config::default();
        let mut form = SettingsForm::from_config(&cfg);
        form.font_size = 400;
        assert!(form.into_config(&cfg).is_err());
        // `-1` is what an unparseable box reads back as.
        form.font_size = -1;
        assert!(form.into_config(&cfg).is_err());
        // The two bounds themselves are fine.
        for ok in [9, 72] {
            form.font_size = ok;
            assert_eq!(form.into_config(&cfg).unwrap().theme.font_size, ok as u32);
        }
    }

    #[test]
    fn a_quota_switched_on_without_a_number_is_refused() {
        let cfg = Config::default();
        let mut form = SettingsForm::from_config(&cfg);
        form.quota_on = true;
        // `NaN` is what an empty or unparseable box reads back as.
        form.quota = f64::NAN;
        assert!(form.into_config(&cfg).is_err());
        form.quota = 0.0;
        assert!(form.into_config(&cfg).is_err(), "zero is not a plan");
        form.quota = -5.0;
        assert!(form.into_config(&cfg).is_err());
        form.quota = 20.0;
        assert_eq!(form.into_config(&cfg).unwrap().quota_gb, 20.0);
    }

    #[test]
    fn the_refresh_period_is_clamped_rather_than_refused() {
        // A rate is a bound, not a preference: a 5 ms refresh is a request for
        // a busy loop, and it becomes the floor instead of an error.
        let cfg = Config::default();
        let mut form = SettingsForm::from_config(&cfg);
        form.interval = 5;
        assert_eq!(form.into_config(&cfg).unwrap().interval_ms, 100);
        form.interval = 999_999;
        assert_eq!(form.into_config(&cfg).unwrap().interval_ms, 10_000);
        form.interval = 2000;
        assert_eq!(form.into_config(&cfg).unwrap().interval_ms, 2000);
    }

    #[test]
    fn the_quota_box_drops_a_pointless_decimal() {
        assert_eq!(format_quota(20.0), "20");
        assert_eq!(format_quota(20.5), "20.5");
        assert_eq!(format_quota(QUOTA_MIN_GB), "0.5");
    }

    #[test]
    fn unparseable_field_text_is_not_a_zero() {
        // The difference matters: `0` is a value the page would save, and an
        // empty box is not.
        assert_eq!(parse_int(None), None);
        assert_eq!(parse_int(Some("")), None);
        assert_eq!(parse_int(Some("abc")), None);
        assert_eq!(parse_int(Some(" 12 ")), Some(12));
        assert_eq!(parse_f64(Some("")), None);
        assert_eq!(parse_f64(Some("1.5")), Some(1.5));
        assert_eq!(parse_int(Some("1.5")), None, "a size is whole");
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
        assert_eq!(
            net,
            vec![
                "Network",
                "Adapter",
                "IP",
                "DNS",
                "Wi-Fi",
                "Download",
                "Upload",
                "Gateway",
                "Latency",
                "Internet",
                "Loss"
            ]
        );
        // None of that detail may leak onto the page people open for the
        // numbers — that was the point of giving it a page at all.
        let overview = labels(OVERVIEW, &full());
        for leaked in [
            "Network", "Adapter", "IP", "DNS", "Wi-Fi", "Gateway", "Internet", "Loss",
        ] {
            assert!(!overview.contains(&leaked), "{leaked} leaked onto Overview");
        }
    }

    #[test]
    fn the_local_ip_reads_before_the_gateway_it_belongs_to() {
        // This machine before the router it talks to: the two are the same
        // reading from either end, and a page that shows one without the other
        // leaves the reader to work out which end they are looking at.
        let rows = page_rows(NETWORK, &full());
        let at = |label| rows.iter().position(|(l, _)| *l == label).unwrap();
        assert!(at("Adapter") < at("IP"), "the address needs its subject first");
        assert!(at("IP") < at("Gateway"));
        assert_eq!(rows[at("IP")].1, "192.168.1.10");
        assert_eq!(rows[at("Gateway")].1, "192.168.1.1");
        // Different machines, so the two rows must never carry the same text.
        assert_ne!(rows[at("IP")].1, rows[at("Gateway")].1);
    }

    #[test]
    fn an_adapter_with_no_address_still_names_itself() {
        // A stack mid-DHCP, or IPv6-only: the adapter is worth a row even when
        // there is no IPv4 to put under it, so the row is dropped on its own
        // rather than taking the name down with it.
        let model = TrayModel {
            adapter_text: "Ethernet".into(),
            ..Default::default()
        };
        let rows = page_rows(NETWORK, &model);
        assert_eq!(rows, vec![("Adapter", "Ethernet".to_string())]);
        assert!(page_rows(NETWORK, &TrayModel::default()).is_empty());
    }

    #[test]
    fn the_system_page_leads_with_the_live_metrics() {
        // The page grew from two rows to eleven. The live pair stays at the top
        // because it is what moves — everything below it is a reading of
        // something that does not, and burying the moving numbers under the
        // machine's name would make the page's most useful line its hardest to
        // find.
        //
        // `Version` is last, and it is the one row here that is always present
        // on every machine: the build line belongs at the foot, after the
        // machine has been described, because it is a fact about the program
        // rather than about the hardware.
        let rows = labels(SYSTEM, &full());
        assert_eq!(&rows[..2], ["CPU", "RAM"]);
        assert_eq!(rows.last(), Some(&"Version"));
        assert_eq!(rows.len(), 11, "the detail rows are missing: {rows:?}");
    }

    #[test]
    fn the_daily_total_has_a_page_of_its_own() {
        let data = page_rows(DATA, &full());
        assert_eq!(data[0], ("Today", "1.4G".to_string()));
        assert_eq!(data[1], ("Month", "41.2G".to_string()));
        assert!(page_shows_graph(DATA), "the total is a traffic story too");
    }

    #[test]
    fn a_partial_month_says_so_in_its_label() {
        // The sum only covers the days the file still holds. The caveat has to
        // live in the caption: a number the reader has to decode twice is a
        // number they misread, and "41.2G" with no qualifier claims a month.
        let partial = TrayModel {
            month_partial: true,
            ..full()
        };
        assert_eq!(labels(DATA, &partial)[1], "Month so far");
        assert_eq!(labels(DATA, &full())[1], "Month");
    }

    #[test]
    fn the_data_page_lists_the_recent_days_oldest_first() {
        let model = TrayModel {
            usage_days: vec![
                ("09-11".into(), 1),
                ("09-12".into(), 2),
                ("09-13".into(), 3),
                ("09-14".into(), 4),
            ],
            ..full()
        };
        assert_eq!(
            usage_rows(&model),
            vec![
                ("09-11".to_string(), 1),
                ("09-12".to_string(), 2),
                ("09-13".to_string(), 3),
                ("09-14".to_string(), 4),
            ]
        );

        // A full window keeps only the newest `USAGE_ROWS`, so the page cannot
        // grow past the bottom of the window.
        let long: Vec<(String, u64)> = (0..30)
            .map(|i| (format!("09-{i:02}"), i as u64))
            .collect();
        let model = TrayModel {
            usage_days: long,
            ..full()
        };
        let rows = usage_rows(&model);
        assert_eq!(rows.len(), USAGE_ROWS);
        assert_eq!(rows.first().unwrap().0, "09-23", "only the newest survive");
        assert_eq!(rows.last().unwrap().0, "09-29", "the newest day must stay");
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
        // `WM_COMMAND` low word is the child id and the high word is the
        // notification, which is why the id has to be masked off before it is
        // compared: an unmasked `wparam` would never equal any control's id.
        // 0 in the high half is `BN_CLICKED`, which is what a checkbox sends.
        let packed = SET_QUOTA_ON as usize;
        assert_eq!(low_word(packed) as i32, SET_QUOTA_ON);
        // A different control's click must not reach the quota switch.
        assert_ne!(low_word(packed) as i32, SET_QUOTA);
        assert_eq!(low_word(SET_SAVE as usize) as i32, SET_SAVE);
    }

    #[test]
    fn a_click_off_the_sidebar_does_not_change_the_page() {
        // The sidebar is a strip down the left. Reading a coordinate as
        // unsigned would turn a click to its left — or a move above the window
        // — into a large positive `x`, and the entry under it would be whatever
        // arithmetic happened to land on. Both halves of a mouse message have
        // to be sign-extended before they are used.
        let sign_extend = |low: i64| (low & 0xFFFF) as u16 as i16 as i32;
        assert_eq!(sign_extend(-1 & 0xFFFF), -1);
        assert_eq!(sign_extend(-4000 & 0xFFFF), -4000);
        assert_eq!(sign_extend(150), 150);
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
    fn the_initial_height_opens_with_room_for_the_last_settings_row() {
        // The Settings page is the tallest, and its last row is placed from the
        // row table rather than pinned to the foot — so a row added without
        // `START_H` moving would open the window with that field under the
        // frame's edge, and nothing else would notice.
        let last = form_top(96) + ROW_H * ROW_SAVE as i32;
        assert!(
            last + ROW_H < START_H,
            "START_H {START_H} leaves the last row ({last}) no room"
        );
    }

    #[test]
    fn the_window_is_sized_for_its_client_area_not_its_frame() {
        // The bug this catches: `START_W`/`START_H` are client-area metrics —
        // `form_top` counts them from the top of the client area — but
        // `CreateWindowExW` takes the size of the whole window. Passing them
        // straight through opened the window a caption and two borders short of
        // what the layout believed it had, which put the Save row 16 pixels
        // under the bottom edge. Nothing in the layout can notice, because the
        // layout is right; only the conversion to an outer size was missing.
        let style = WINDOW_STYLE(WS_OVERLAPPEDWINDOW.0 | WS_CLIPCHILDREN.0);
        let (w, h) = outer_size(style, WINDOW_EX_STYLE(0), 96);
        assert!(h > START_H, "outer height {h} must exceed the client {START_H}");
        assert!(w > START_W, "outer width {w} must exceed the client {START_W}");
        // A caption bar is not incidental: if this ever came back equal, the
        // call had stopped adjusting and the frame is being drawn over again.
        assert!(h - START_H > 20, "room for the caption bar, got {}", h - START_H);
    }

    #[test]
    fn every_settings_row_has_a_caption_on_its_own_band() {
        // The captions are painted in order beside controls placed by row
        // index, so the two only agree while every row index below the count
        // is used by exactly the controls `FIELD_ROWS` gives it.
        for (id, row) in FIELD_ROWS {
            assert!(row < SET_ROW_COUNT, "control {id} is off the page");
            assert!(
                !SET_ROW_LABELS[row].is_empty(),
                "control {id} sits on a band with no caption"
            );
        }
    }

    #[test]
    fn a_caption_drops_to_meet_the_control_beside_it() {
        // Measured on the running page: a native control centres its own text
        // while a row draws from the top of its band, which left every label
        // on the form five pixels above the value next to it.
        assert!(FIELD_DROP > 0, "a field row has to drop to meet its control");
        assert_eq!(FIELD_DROP, 5, "re-measure if the control font or CTL_H moved");
        // Every band that carries a control drops, and only those: the divider
        // claims a band of its own, and moving its caption would slide the rule
        // off the band it was given.
        for (id, row) in FIELD_ROWS {
            assert_eq!(field_drop(row, 96), FIELD_DROP, "control {id} on row {row}");
        }
        assert_eq!(field_drop(ROW_DIVIDER, 96), 0, "the rule must not move");
        assert_eq!(field_drop(ROW_REFRESH, 96), FIELD_DROP, "row 5 has a field");
    }

    #[test]
    fn the_page_buttons_sit_inside_the_window() {
        // The Stopwatch pair used to be wrapped onto a second row at the foot
        // of the content column. A second row there starts *below* the client
        // area, so both buttons were drawn 14 pixels off the bottom edge — and
        // nothing could notice: `place` is one `SetWindowPos`, and a child
        // window positioned past its parent's edge is not an error to Windows.
        // The arithmetic is therefore checked here, where there is no window.
        let dpi = 96;
        let x0 = scale(SIDEBAR_W, dpi) + scale(PAD, dpi);
        let field_w = scale(FIELD_W, dpi);
        let gap = scale(FIELD_GAP, dpi);
        let ctl_h = scale(CTL_H, dpi);
        let foot = foot_button_top(START_H, dpi);

        // Every one of the four is placed on every layout, and only one pair is
        // ever shown, so the two columns are what has to be told apart.
        let left = foot_slot(0, x0, field_w, gap, foot, ctl_h);
        let right = foot_slot(1, x0, field_w, gap, foot, ctl_h);

        assert!(
            left.1 + ctl_h <= START_H,
            "the foot row runs to {} in a {START_H}-tall window",
            left.1 + ctl_h
        );
        assert!(
            left.0 + field_w <= right.0,
            "the two columns meet: {} then {}",
            left.0 + field_w,
            right.0
        );
        // And the pair has to fit the content column as well as the window.
        assert!(
            right.0 + field_w <= START_W - scale(PAD, dpi),
            "the foot row runs past the content column"
        );
        // Below the heading, or the button is drawn over the page's own title.
        assert!(foot >= form_top(dpi), "the foot row is above the first band");
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
