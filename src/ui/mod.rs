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

//! The tree mirrors the file this came from: the window's lifecycle lives here,
//! the theme, fonts and metrics have a module each, and the pages, painter,
//! sidebar and settings controls sit beside them.

mod consts;
mod fonts;
mod layout;
mod pages;
mod paint;
mod settings;
mod sidebar;
mod theme;

pub(crate) use consts::*;
pub(crate) use fonts::*;
pub(crate) use layout::*;
pub(crate) use pages::*;
pub(crate) use paint::*;
pub(crate) use settings::*;
pub(crate) use sidebar::*;
pub(crate) use theme::*;

use crate::config::Config;
use crate::taskbar::TrayModel;
use crate::taskbar::icon::app_icon;
use crate::taskbar::render::parse_color;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, HBRUSH, HGDIOBJ, HFONT, InvalidateRect,
    PAINTSTRUCT, SetBkColor, SetTextColor,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA,
    GetSystemMetrics, GetWindowLongPtrW, IsIconic, IsWindow, LB_ERR, LB_GETCURSEL, LBN_SELCHANGE,
    MINMAXINFO, MoveWindow, RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_RESTORE,
    SW_SHOW, SendMessageW, SetForegroundWindow, SetWindowLongPtrW, ShowWindow, WM_CLOSE,
    WM_COMMAND, WM_CREATE, WM_CTLCOLORBTN, WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC,
    WM_DPICHANGED, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SIZE,
    WNDCLASSW, WINDOW_EX_STYLE, WINDOW_STYLE, WS_CLIPCHILDREN, WS_OVERLAPPEDWINDOW,
};
use windows::core::w;

/// What the window knows between repaints. Boxed and hung off the window's
/// `GWLP_USERDATA`, so there is no global and no lifetime to get wrong.
pub(crate) struct UiState {
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
        face_brush: unsafe { CreateSolidBrush(bg) },
        instance,
        settings: SettingsForm::from_config(cfg),
        notice: None,
        handed_back: false,
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
///
/// Returns a config to adopt instead of the one the tray is running with, and
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
                    create_settings(hwnd, s);
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

            // Both the page list and the Settings page's own controls arrive
            // here. The list is dispatched on its id and its notification; the
            // settings controls have no notification worth filtering on, so
            // their id alone is the test.
            WM_COMMAND => {
                if !state.is_null() {
                    let s = &mut *state;
                    let id = control_id(wparam);
                    if id == LIST_ID && high_word(wparam.0) as u32 == LBN_SELCHANGE {
                        if !s.list.is_invalid() {
                            let picked = SendMessageW(s.list, LB_GETCURSEL, None, None).0 as i32;
                            if picked != LB_ERR && picked >= 0 {
                                s.page = picked as usize;
                                // The controls belong to one page and are
                                // clipped to their own rects, not to the page
                                // they are on: leaving them up would paint
                                // eight checkboxes over the sparkline.
                                show_settings(hwnd, s.page == SETTINGS);
                                let _ = InvalidateRect(Some(hwnd), None, false);
                            }
                        }
                    } else if id == SET_QUOTA_ON {
                        // Disabled rather than silently ignored: with the
                        // switch off the number has no meaning, and leaving it
                        // editable would suggest it does.
                        set_enabled(hwnd, SET_QUOTA, is_checked(hwnd, SET_QUOTA_ON));
                        s.notice = None;
                    } else if id == SET_SAVE {
                        let notice = save_settings(hwnd, s);
                        s.notice = Some(notice);
                        // The notice is painted by us, so nothing repaints it
                        // on its own — and a save that changed the theme has
                        // to show its own result here too.
                        repaint_after_settings(hwnd, s);
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
                    let _ = DeleteObject(HGDIOBJ(s.side_brush.0));
                    let _ = DeleteObject(HGDIOBJ(s.face_brush.0));
                }
                LRESULT(0)
            }

            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// Low and high halves of a `WPARAM`, which is where `WM_COMMAND` packs the
/// child id and the notification code. `LOWORD`/`HIWORD` are not in the
/// bindings, and the masks are the whole of them.
pub(crate) fn low_word(value: usize) -> u16 {
    (value & 0xFFFF) as u16
}

pub(crate) fn high_word(value: usize) -> u16 {
    ((value >> 16) & 0xFFFF) as u16
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

        // Nothing may collide with the page list or with a tile checkbox.
        for id in &seen {
            assert_ne!(*id, LIST_ID);
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

        let back = crate::config::Show::default();
        assert_eq!(tile_flags(&back), [true, true, true, true, true, false, false, true]);
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
    fn the_system_page_is_only_hardware() {
        assert_eq!(labels(SYSTEM, &full()), vec!["CPU", "RAM"]);
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
