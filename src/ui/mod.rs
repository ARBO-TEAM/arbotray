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

use crate::config::{
    Config, QUOTA_MAX_GB, QUOTA_MIN_GB, clamp_font_size, clamp_interval_ms, clamp_quota_gb,
};
use crate::taskbar::TrayModel;
use crate::taskbar::icon::app_icon;
use crate::taskbar::render::{parse_color, sparkline_points};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreatePen, CreateSolidBrush,
    DEFAULT_CHARSET, DEFAULT_GUI_FONT, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT,
    DT_SINGLELINE, DeleteObject,
    DrawTextW, EndPaint, FW_NORMAL, FillRect, GetDC, GetStockObject, HBRUSH, HGDIOBJ, HFONT,
    InvalidateRect, NULL_BRUSH, OUT_DEFAULT_PRECIS, PAINTSTRUCT, PS_SOLID, Polyline, ReleaseDC,
    SelectObject, SetBkColor, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BS_AUTOCHECKBOX, BS_PUSHBUTTON, BM_GETCHECK, BM_SETCHECK, CREATESTRUCTW, CW_USEDEFAULT,
    CreateWindowExW, DefWindowProcW, DestroyWindow, ES_AUTOHSCROLL, ES_NUMBER, GWLP_USERDATA,
    GetClientRect, GetDlgItem, GetSystemMetrics, GetWindowLongPtrW, GetWindowTextW,
    GetWindowTextLengthW, HMENU, IsIconic, IsWindow, LB_ADDSTRING, LB_ERR, LB_GETCURSEL,
    LB_SETCURSEL, LBS_NOTIFY, LBS_NOINTEGRALHEIGHT, LBN_SELCHANGE, MINMAXINFO, MoveWindow,
    RegisterClassW, SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_RESTORE, SW_SHOW, SWP_NOACTIVATE,
    SWP_NOZORDER, SendMessageW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos,
    SetWindowTextW, ShowWindow, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLORBTN, WM_CTLCOLOREDIT,
    WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_DPICHANGED, WM_ERASEBKGND, WM_GETMINMAXINFO,
    WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SETFONT, WM_SIZE, WNDCLASSW, WINDOW_EX_STYLE,
    WINDOW_STYLE, WS_BORDER, WS_CHILD, WS_CLIPCHILDREN, WS_OVERLAPPEDWINDOW, WS_TABSTOP,
    WS_VISIBLE, WS_VSCROLL,
};

use windows::core::{PCWSTR, PWSTR, w};

/// The window class, registered on first show and reused after.
const CLASS: PCWSTR = w!("ArboTrayWindow");

/// Child id of the page list, handed to `CreateWindowExW` as an `HMENU` and
/// read back out of the low word of `WM_COMMAND`'s `wparam`.
const LIST_ID: i32 = 1;

/// Where the Settings page's controls start. Deliberately far above `LIST_ID`:
/// the list owns 1 and nothing else may take it, and a gap leaves room for
/// another control on an existing page without renumbering anything.
///
/// The Settings controls are addressed by id rather than by remembered handle
/// values, because hiding and showing the page is a message away and the
/// handles are read back with `GetDlgItem`. Nothing needs to keep them here.
const SET_ID_BASE: i32 = 10;

/// The tile checkboxes' ids, in `TILE_LABELS` order. Laying the grid out from
/// this and reading it back in the same order is what keeps the nth checkbox
/// the nth tile: there is no name-to-id lookup to get wrong.
const TILE_IDS: [i32; 8] = [
    SET_ID_BASE,
    SET_ID_BASE + 1,
    SET_ID_BASE + 2,
    SET_ID_BASE + 3,
    SET_ID_BASE + 4,
    SET_ID_BASE + 5,
    SET_ID_BASE + 6,
    SET_ID_BASE + 7,
];
const SET_INTERVAL: i32 = SET_ID_BASE + 8;
const SET_QUOTA_ON: i32 = SET_ID_BASE + 9;
const SET_QUOTA: i32 = SET_ID_BASE + 10;
const SET_FONT: i32 = SET_ID_BASE + 11;
const SET_BG: i32 = SET_ID_BASE + 12;
const SET_FG: i32 = SET_ID_BASE + 13;
const SET_ALERT: i32 = SET_ID_BASE + 14;
const SET_OPACITY: i32 = SET_ID_BASE + 15;
const SET_SAVE: i32 = SET_ID_BASE + 16;
const SET_RESET: i32 = SET_ID_BASE + 17;

/// Nothing in the page is live until Save runs, so the page has to say so.
///
/// `WM_ENABLE` is the one control message the `WindowsAndMessaging` bindings
/// do not expose as a named constant, and `EnableWindow` is not there either.
/// It is a documented, stable message number, so it is spelled out rather than
/// pulling in `Win32_UI_Controls` for it.
const WM_ENABLE: u32 = 0x000A;

/// The button check state `BM_GETCHECK` returns for a ticked checkbox.
///
/// `BST_CHECKED` lives in `Win32_UI_Controls` with the rest of the owner-draw
/// machinery, and enabling that feature to read one `1` is not worth it. The
/// unchecked and indeterminate states are `0` and `2`; only the first two are
/// used here, and the unchecked case is `0`, so nothing spells it out.
const BST_CHECKED: isize = 1;

/// Text height of a settings control at 96 DPI, and the nudge that lines its
/// text up with the painted rows. A native control centres its text in its own
/// rect while the content painter draws from the top, so a control put on the
/// same band as a row sits a couple of pixels low without this.
const CTL_H: i32 = 24;
const CTL_NUDGE: i32 = 3;

/// Width of one field control — an edit box or the Save button — at 96 DPI.
/// Fixed rather than proportional: at the minimum window width the content
/// column is narrow enough that a proportional field would clip `#E6E6E6`.
const FIELD_W: i32 = 150;

/// The taskbar tiles' checkbox labels, in `TILE_IDS` order. They are the
/// config file's own field names, so what the page shows is what the file says.
const TILE_LABELS: [&str; 8] = [
    "net_down", "net_up", "latency", "cpu", "ram", "wifi", "usage", "sparkline",
];

/// The pages, in the order the list shows them. Their indices are the page
/// numbers used throughout, so the constants below name the slots rather than
/// leaving magic numbers in the row functions.
///
/// `Settings` is **appended**. These indices are positional, so inserting it
/// anywhere but the end would renumber every page after it — the labels would
/// still read correctly and the routing would be wrong.
const PAGES: [&str; 5] = ["Overview", "Network", "System", "Data", "Settings"];
const OVERVIEW: usize = 0;
const NETWORK: usize = 1;
const SYSTEM: usize = 2;
const DATA: usize = 3;
const SETTINGS: usize = 4;

/// Everything the Settings page knows, in the form it knows it: text exactly as
/// typed, and integers parsed with a running fallback.
///
/// The page never holds a `Config` while the user is typing. Blanking one digit
/// on a numeric field is a normal thing to do, and a `Config` cannot represent
/// "no number yet" — an unparseable field would have to be either a `0` that
/// erases the setting or an error that blocks the other six. Keeping the raw
/// strings here means `into_config` is the one place a typed value becomes a
/// setting, and it is a pure function that can be tested on its own.
struct SettingsForm {
    /// `Show` flags, in `TILE_LABELS` order.
    tiles: [bool; 8],
    interval: i32,
    quota_on: bool,
    quota: f64,
    font_size: i32,
    background: String,
    foreground: String,
    alert: String,
    opacity: i32,
}

impl SettingsForm {
    /// A form showing `cfg` — what the page loads on creation.
    fn from_config(cfg: &Config) -> Self {
        Self {
            tiles: tile_flags(&cfg.show),
            interval: clamp_interval_ms(cfg.interval_ms) as i32,
            // `0` is the config's own "no plan" value, so it is displayed as
            // the switch being off rather than as a number in the box.
            quota_on: cfg.quota_gb > 0.0,
            quota: if cfg.quota_gb > 0.0 {
                cfg.quota_gb
            } else {
                QUOTA_MIN_GB
            },
            font_size: clamp_font_size(cfg.theme.font_size) as i32,
            background: cfg.theme.background.clone(),
            foreground: cfg.theme.foreground.clone(),
            alert: cfg.theme.alert.clone(),
            opacity: cfg.theme.opacity as i32,
        }
    }

    /// The config this form describes: every field validated here, so the rest
    /// of the app only ever sees a setting it can act on.
    ///
    /// Two fields are deliberately *clamped* rather than refused — the refresh
    /// period, because a rate is a bound and not a preference, and the quota,
    /// because a plan of zero is not a plan. The two that are refused are the
    /// font size and the quota-when-switched-on: clamping those would let a
    /// typo silently become a number the user never typed, and "100000" in the
    /// size box would come back as "72" with nothing said.
    fn into_config(&self, base: &Config) -> Result<Config, String> {
        let font_size = validate_font_size(self.font_size)?;
        let quota_on = validate_quota_on(self.quota_on, self.quota)?;

        let mut cfg = base.clone();
        apply_tiles(&mut cfg.show, &self.tiles);
        cfg.interval_ms = clamp_interval_ms(self.interval.max(0) as u32);
        cfg.quota_gb = quota_on.map_or(0.0, |gb| gb);
        cfg.theme.font_size = font_size;
        cfg.theme.background = self.background.trim().to_string();
        cfg.theme.foreground = self.foreground.trim().to_string();
        cfg.theme.alert = self.alert.trim().to_string();
        cfg.theme.opacity = self.opacity.clamp(0, 255) as u8;
        Ok(cfg)
    }
}

/// Whether each tile is switched on, in `TILE_LABELS` order.
///
/// `Show` has no iterator over its eight `bool`s, so this mapping is written
/// out once here. It is exhaustive on both sides, which is what stops a field
/// being silently dropped from the page: `apply_tiles` is the inverse, and the
/// round-trip test over them fails the moment the two stop agreeing.
fn tile_flags(show: &crate::config::Show) -> [bool; 8] {
    [
        show.net_down,
        show.net_up,
        show.latency,
        show.cpu,
        show.ram,
        show.wifi,
        show.usage,
        show.sparkline,
    ]
}

/// The inverse of `tile_flags`.
fn apply_tiles(show: &mut crate::config::Show, tiles: &[bool; 8]) {
    show.net_down = tiles[0];
    show.net_up = tiles[1];
    show.latency = tiles[2];
    show.cpu = tiles[3];
    show.ram = tiles[4];
    show.wifi = tiles[5];
    show.usage = tiles[6];
    show.sparkline = tiles[7];
}

/// The font size, or the message to show instead of saving one this app cannot
/// build. The bound is shared with the window's own font helper.
fn validate_font_size(size: i32) -> Result<u32, String> {
    if size <= 0 {
        return Err("font size must be a whole number".into());
    }
    let size = size as u32;
    if clamp_font_size(size) != size {
        return Err(format!("font size must be between 9 and 72, not {size}"));
    }
    Ok(size)
}

/// The quota, or the message to show. `None` is the quota switched off, which
/// is a valid answer meaning "no plan" — not an error.
fn validate_quota_on(on: bool, gb: f64) -> Result<Option<f64>, String> {
    if !on {
        return Ok(None);
    }
    clamp_quota_gb(gb).map(Some).ok_or_else(|| {
        format!(
            "quota must be a number between {QUOTA_MIN_GB} and {QUOTA_MAX_GB:.0} GB, or leave it off"
        )
    })
}

/// What the user left on the Settings page, read back out of the live controls.
///
/// This is the one piece of window state that is not in `UiState`: the values
/// live in native controls, which keep their own state and would lose it if
/// this were mirrored on every `WM_COMMAND`. It is only ever built while the
/// window exists, from an `hwnd` that has already been checked.
fn read_form(hwnd: HWND) -> SettingsForm {
    SettingsForm {
        tiles: std::array::from_fn(|i| is_checked(hwnd, TILE_IDS[i])),
        // An unreadable number becomes something `into_config` refuses rather
        // than a zero it would quietly save: `0` and "not a number" have to
        // stay different.
        interval: parse_int(text_of(hwnd, SET_INTERVAL).as_deref()).unwrap_or(0),
        quota_on: is_checked(hwnd, SET_QUOTA_ON),
        quota: parse_f64(text_of(hwnd, SET_QUOTA).as_deref()).unwrap_or(f64::NAN),
        font_size: parse_int(text_of(hwnd, SET_FONT).as_deref()).unwrap_or(-1),
        background: text_of(hwnd, SET_BG).unwrap_or_default(),
        foreground: text_of(hwnd, SET_FG).unwrap_or_default(),
        alert: text_of(hwnd, SET_ALERT).unwrap_or_default(),
        opacity: parse_int(text_of(hwnd, SET_OPACITY).as_deref()).unwrap_or(-1),
    }
}

/// Whether one of our checkboxes is ticked. A missing control reads as unticked
/// rather than as an error: the control failing to exist is a layout problem,
/// not a settings one.
fn is_checked(hwnd: HWND, id: i32) -> bool {
    // SAFETY: `ctl` is a control of ours; `BM_GETCHECK` needs no buffer and
    // returns the state directly.
    unsafe {
        GetDlgItem(Some(hwnd), id).ok().is_some_and(|ctl| {
            SendMessageW(ctl, BM_GETCHECK, None, None).0 == BST_CHECKED
        })
    }
}

/// Push a config into the live controls. Used to load the page and to undo a
/// failed save, so the page never shows one thing and believes another.
fn write_form(hwnd: HWND, cfg: &Config) {
    let form = SettingsForm::from_config(cfg);
    for (i, id) in TILE_IDS.iter().enumerate() {
        set_check(hwnd, *id, form.tiles[i]);
    }
    set_text(hwnd, SET_INTERVAL, &form.interval.to_string());
    set_check(hwnd, SET_QUOTA_ON, form.quota_on);
    set_text(hwnd, SET_QUOTA, &format_quota(form.quota));
    set_text(hwnd, SET_FONT, &form.font_size.to_string());
    set_text(hwnd, SET_BG, &form.background);
    set_text(hwnd, SET_FG, &form.foreground);
    set_text(hwnd, SET_ALERT, &form.alert);
    set_text(hwnd, SET_OPACITY, &form.opacity.to_string());
    // The quota field is only meaningful while its switch is on.
    set_enabled(hwnd, SET_QUOTA, form.quota_on);
}

/// The quota as it appears in the edit box: one decimal for a real plan, and
/// no trailing `.0` on a whole one, because `20.0` in a field that takes a
/// decimal is a formatting choice the user would not have made.
fn format_quota(gb: f64) -> String {
    if gb.fract() == 0.0 {
        format!("{gb:.0}")
    } else {
        format!("{gb:.1}")
    }
}

/// A quota is typed with a decimal point, so it cannot be parsed as an integer.
fn parse_f64(text: Option<&str>) -> Option<f64> {
    text?.trim().parse().ok()
}

/// `None` when the field holds something that is not a number, which every
/// caller treats as "unusable" rather than as zero.
fn parse_int(text: Option<&str>) -> Option<i32> {
    text?.trim().parse().ok()
}

/// The current text of one of our own edit controls, or `None` when the
/// control is missing or holds something that is not valid UTF-16.
fn text_of(hwnd: HWND, id: i32) -> Option<String> {
    // SAFETY: `hwnd` is our own live window, so `GetDlgItem` only looks up a
    // child of it. The buffer is a live local and the length is read from the
    // control rather than assumed, so a long paste cannot overrun it.
    unsafe {
        let ctl = GetDlgItem(Some(hwnd), id).ok()?;
        let len = GetWindowTextLengthW(ctl);
        if len <= 0 {
            return Some(String::new());
        }
        let mut buf = vec![0u16; len as usize + 1];
        let copied = GetWindowTextW(ctl, &mut buf);
        if copied <= 0 {
            return Some(String::new());
        }
        buf.truncate(copied as usize);
        Some(String::from_utf16_lossy(&buf))
    }
}

/// Set an edit control's text. A failure is ignored: the control is either
/// missing, in which case there is nothing to set, or it took some of the text
/// and the next load fixes it.
fn set_text(hwnd: HWND, id: i32, value: &str) {
    // SAFETY: the id names one of our own children, and `value` outlives the
    // call — `SetWindowTextW` copies it.
    unsafe {
        if let Ok(ctl) = GetDlgItem(Some(hwnd), id) {
            let mut wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
            let _ = SetWindowTextW(ctl, PWSTR(wide.as_mut_ptr()));
        }
    }
}

fn set_check(hwnd: HWND, id: i32, value: bool) {
    // SAFETY: `BM_SETCHECK` takes the state by value and needs no buffer.
    unsafe {
        if let Ok(ctl) = GetDlgItem(Some(hwnd), id) {
            let state = if value { BST_CHECKED } else { 0 };
            SendMessageW(ctl, BM_SETCHECK, Some(WPARAM(state as usize)), None);
        }
    }
}

/// Grey out a control that has no effect in the current state.
fn set_enabled(hwnd: HWND, id: i32, enabled: bool) {
    // SAFETY: `WM_ENABLE` reads its state from `wparam` and touches nothing
    // else on our own child.
    unsafe {
        if let Ok(ctl) = GetDlgItem(Some(hwnd), id) {
            SendMessageW(ctl, WM_ENABLE, Some(WPARAM(enabled as usize)), None);
        }
    }
}

/// The child id a `WM_COMMAND` carries in the low word of `wparam`.
fn control_id(wparam: WPARAM) -> i32 {
    low_word(wparam.0) as i32
}

// --- settings rows --------------------------------------------------------

/// Create the Settings page's controls, hidden.
///
/// They are children of the frame, not of a container, because a container is
/// another window to own, size and paint for no gain: `WS_CLIPCHILDREN` on the
/// frame already stops its own repaints reaching them, and the hiding below is
/// what keeps them off the other four pages.
///
/// Every one of them is created with `WS_VISIBLE` **absent** and shown only
/// when `SETTINGS` is the page. A control created visible would flash over
/// `Overview` for the first frame.
fn create_settings(parent: HWND, state: &mut UiState) {
    // The `BS_`/`ES_` flags are plain `i32` in the bindings while `WS_*` are a
    // newtype, hence the mixed casts — same as the sidebar's.
    let check_style = WINDOW_STYLE(WS_CHILD.0 | (BS_AUTOCHECKBOX as u32) | WS_TABSTOP.0);
    for (i, id) in TILE_IDS.iter().enumerate() {
        create_control(parent, state, w!("BUTTON"), TILE_LABELS[i], check_style, *id);
    }

    // `ES_LEFT` is 0 in the bindings, so it is not spelled out here.
    let edit_style = WINDOW_STYLE(WS_CHILD.0 | WS_BORDER.0 | (ES_AUTOHSCROLL as u32) | WS_TABSTOP.0);
    // The quota is a decimal, the rest are whole numbers. `ES_NUMBER` rejects
    // the keystroke rather than the value, which is the right behaviour for a
    // field a user is typing into.
    let num_style = WINDOW_STYLE(edit_style.0 | ES_NUMBER as u32);
    create_control(parent, state, w!("EDIT"), "", edit_style, SET_BG);
    create_control(parent, state, w!("EDIT"), "", edit_style, SET_FG);
    create_control(parent, state, w!("EDIT"), "", edit_style, SET_ALERT);
    create_control(parent, state, w!("EDIT"), "", num_style, SET_INTERVAL);
    create_control(parent, state, w!("EDIT"), "", edit_style, SET_QUOTA);
    create_control(parent, state, w!("EDIT"), "", num_style, SET_FONT);
    create_control(parent, state, w!("EDIT"), "", num_style, SET_OPACITY);

    create_control(parent, state, w!("BUTTON"), "enable", check_style, SET_QUOTA_ON);
    let button_style = WINDOW_STYLE(WS_CHILD.0 | (BS_PUSHBUTTON as u32) | WS_TABSTOP.0);
    create_control(parent, state, w!("BUTTON"), "Save", button_style, SET_SAVE);
    create_control(parent, state, w!("BUTTON"), "Reload", button_style, SET_RESET);

    write_form(parent, &state.cfg);
}

/// One child control, hidden until its page is showing.
fn create_control(
    parent: HWND,
    state: &UiState,
    class: PCWSTR,
    text: &str,
    style: WINDOW_STYLE,
    id: i32,
) {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `parent` is our own window and `class`/`wide` outlive the call.
    unsafe {
        let _ = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            PCWSTR(wide.as_ptr()),
            style,
            0,
            0,
            0,
            0,
            Some(parent),
            Some(HMENU(id as *mut core::ffi::c_void)),
            Some(state.instance),
            None,
        );
    }
}


/// Columns the tile checkboxes are laid out in. Two fits the eight tiles into
/// four rows, which is what keeps the whole page inside the minimum window
/// height without a scrollbar — and the minimum is a floor people actually
/// drag the frame to.
const TILE_COLS: usize = 2;
/// Gap between the tile columns, and between a caption's field and the value
/// column beside it.
const FIELD_GAP: i32 = 10;

/// Rows the whole settings page occupies. The tile grid takes the first four;
/// every labelled field under it takes one more, and Save takes the last.
const ROW_REFRESH: usize = 4;
const ROW_PLAN: usize = 5;
const ROW_FONT: usize = 6;
const ROW_BG: usize = 7;
const ROW_FG: usize = 8;
const ROW_ALERT: usize = 9;
const ROW_OPACITY: usize = 10;
const ROW_SAVE: usize = 11;
const SET_ROW_COUNT: usize = ROW_SAVE + 1;

/// The Settings page's captions, one per row in paint order, with whatever a
/// bare number would not say for itself. The four rows the tile grid occupies
/// have no caption of their own — a checkbox carries its own label — so they
/// are empty strings rather than a shorter array with an offset to get wrong.
///
/// This is the page's own extension point, the way `page_rows` is for the
/// metric pages: `page_rows` returns nothing here because the page has no
/// metric on it, and every line a user reads is declared in this array.
const SET_ROW_LABELS: [&str; SET_ROW_COUNT] = [
    "",
    "",
    "",
    "",
    "Refresh   ms",
    "Monthly plan   GB",
    "Font size   px",
    "Background   #RRGGBB",
    "Foreground   #RRGGBB",
    "Alert   #RRGGBB",
    "Opacity   0-255",
    "Write config.json",
];

/// Which row each non-tile control belongs on. One table drives both the layout
/// and the captions, so a control cannot end up under the wrong line.
const FIELD_ROWS: [(i32, usize); 10] = [
    (SET_INTERVAL, ROW_REFRESH),
    (SET_QUOTA_ON, ROW_PLAN),
    (SET_QUOTA, ROW_PLAN),
    (SET_FONT, ROW_FONT),
    (SET_BG, ROW_BG),
    (SET_FG, ROW_FG),
    (SET_ALERT, ROW_ALERT),
    (SET_OPACITY, ROW_OPACITY),
    (SET_SAVE, ROW_SAVE),
    (SET_RESET, ROW_SAVE),
];

/// The controls that sit in the value column rather than at the field's own
/// left edge — a second control sharing a row with the first. Save and Reload
/// are the pair; the plan's switch and number are the other.
fn right_hand_control(id: i32) -> bool {
    matches!(id, SET_RESET | SET_QUOTA)
}

/// Which grid slot a tile's checkbox takes: the row under row 0, and the
/// column within the field. Pure, so the packing can be checked without a
/// window — eight tiles in two columns is four rows only if this agrees.
fn tile_slot(index: usize) -> (usize, usize) {
    (index / TILE_COLS, index % TILE_COLS)
}

/// Where a tile checkbox sits: the left and right edges of column `col` inside
/// a field `x0..x1` wide, with `gap` already scaled.
fn tile_column(col: usize, x0: i32, x1: i32, gap: i32) -> (i32, i32) {
    let span = ((x1 - x0) - gap * (TILE_COLS as i32 - 1)) / TILE_COLS as i32;
    let left = x0 + (span + gap) * col as i32;
    (left, left + span)
}

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
            // The interface first, then what is on it: "which adapter is this"
            // is the question every address below is an answer to, and the
            // local IP sits directly above the gateway so the two ends of the
            // connection read as a pair.
            push("Adapter", &model.adapter_text);
            push("IP", &model.ip_text);
            push("DNS", &model.dns_text);
            push("Wi-Fi", &model.wifi_text);
            push("Download", &model.down_text);
            push("Upload", &model.up_text);
            push("Gateway", &model.gateway_text);
            push("Latency", &model.latency_text);
            // Right after the gateway it depends on: they are read together to
            // tell a local fault from a provider one.
            push("Internet", &model.internet_text);
            push("Loss", &model.loss_text);
        }
        SYSTEM => {
            push("CPU", &model.cpu_text);
            push("RAM", &model.ram_text);
        }
        DATA => {
            push("Today", &model.usage_text);
            // The caption carries the caveat, not the number. When the file no
            // longer reaches back to the first of the month the sum is of the
            // recent past only, and "Month so far" says so where a reader will
            // actually look.
            push(month_label(model), &model.month_text);
        }
        // The Settings page has no metric on it: every line it shows is a
        // caption from `SET_ROW_LABELS` beside a control. Without this arm it
        // would fall through to the overview below and paint the traffic
        // figures in the gaps between its own fields.
        SETTINGS => {}
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

/// How many trailing days the Data page lists. The whole retention window is
/// often a month of rows; the last week is what the page is opened to see, and
/// the month total above it already accounts for the rest.
const USAGE_ROWS: usize = 7;

/// The Data page's day rows, oldest first, as `(MM-DD, total)`.
///
/// Rendered here rather than through `page_rows` because their labels are
/// runtime dates, not the `&'static str` captions the metric pages use.
fn usage_rows(model: &TrayModel) -> Vec<(String, u64)> {
    let skip = model.usage_days.len().saturating_sub(USAGE_ROWS);
    model
        .usage_days
        .iter()
        .skip(skip)
        .map(|(day, bytes)| (day.clone(), *bytes))
        .collect()
}

/// The month row's caption. Empty — and therefore rowless — until the counter
/// has a day to belong to.
fn month_label(model: &TrayModel) -> &'static str {
    if model.month_text.is_empty() {
        ""
    } else if model.month_partial {
        "Month so far"
    } else {
        "Month"
    }
}

/// Whether a page has anything for the sparkline to say. The traffic history
/// belongs with the traffic figures, and the daily total is the same story at
/// a coarser grain — the hardware and settings pages have no history to draw.
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
    layout_settings(hwnd, state);
}

/// Put the settings controls on the rows `SET_ROW_LABELS` names, and give them
/// the window's font so they match the painted text beside them.
///
/// The controls are laid out even while hidden: they are hidden with
/// `SW_HIDE`, which leaves a control's rectangle alone, so the next time the
/// page is shown it is already correct — including after a resize or a DPI
/// change that happened while it was off screen.
fn layout_settings(hwnd: HWND, state: &mut UiState) {
    // SAFETY: `hwnd` is our own window and every handle below is one of our own
    // children, looked up by id.
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

        let pad = scale(PAD, state.dpi);
        let side = sidebar_w(state.dpi);
        let x0 = if state.list.is_invalid() { pad } else { side + pad };
        let x1 = w - pad;
        if x1 <= x0 {
            return;
        }
        let row_h = scale(ROW_H, state.dpi);
        let ctl_h = scale(CTL_H, state.dpi);
        let nudge = scale(CTL_NUDGE, state.dpi);
        // The first row of controls sits one title-height below the page
        // heading, exactly where `paint` puts its first row — so the labels and
        // the controls that belong to them share a band.
        let top = pad + scale(ROW_H + TITLE_EXTRA * 2, state.dpi) + scale(VALUE_OFFSET, state.dpi);
        let _ = h;

        let field_w = scale(FIELD_W, state.dpi);
        let gap = scale(FIELD_GAP, state.dpi);

        for (i, id) in TILE_IDS.iter().enumerate() {
            let (row, col) = tile_slot(i);
            let (left, right) = tile_column(col, x0, x1, gap);
            place(hwnd, *id, left, top + row_h * row as i32 + nudge, right - left, ctl_h);
        }

        for (id, row) in FIELD_ROWS {
            let y = top + row_h * row as i32 + nudge;
            // The two right-hand controls share their row with the field that
            // owns it: the plan's switch says whether the number beside it means
            // anything, and Reload sits next to the Save it undoes.
            let (left, width) = if right_hand_control(id) {
                (x0 + field_w + gap, field_w)
            } else {
                (x0, field_w)
            };
            place(hwnd, id, left, y, width, ctl_h);
        }

        for id in control_ids() {
            if let Ok(ctl) = GetDlgItem(Some(hwnd), id) {
                SendMessageW(
                    ctl,
                    WM_SETFONT,
                    Some(WPARAM(state.font.0 as usize)),
                    Some(LPARAM(1)),
                );
            }
        }
    }
}

/// Every control id the Settings page owns, in creation order.
fn control_ids() -> Vec<i32> {
    let mut ids: Vec<i32> = TILE_IDS.to_vec();
    ids.extend(FIELD_ROWS.iter().map(|(id, _)| *id));
    ids
}

/// Move one control into place. `SWP_SHOWWINDOW` is deliberately absent: which
/// page is showing is `show_settings`'s business, not the layout's.
fn place(hwnd: HWND, id: i32, x: i32, y: i32, w: i32, h: i32) {
    // SAFETY: `set_enabled`-style lookup of one of our own children; the call
    // only moves that child.
    unsafe {
        if let Ok(ctl) = GetDlgItem(Some(hwnd), id) {
            let _ = SetWindowPos(ctl, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
}

/// Show or hide the whole Settings page. Called when the page changes: the
/// controls are clipped to their own rectangles by `WS_CLIPCHILDREN`, not to
/// the page they belong to, so leaving them up would paint eight checkboxes
/// over the sparkline on every other page.
fn show_settings(hwnd: HWND, visible: bool) {
    let flag = if visible { SW_SHOW } else { SW_HIDE };
    // SAFETY: every id names one of our own children; `ShowWindow` on a child
    // only changes its visibility.
    unsafe {
        for id in control_ids() {
            if let Ok(ctl) = GetDlgItem(Some(hwnd), id) {
                let _ = ShowWindow(ctl, flag);
            }
        }
    }
}

/// Write the form to disk, then arrange for the tray to pick the change up.
///
/// Three things have to happen and only the first can happen here. The file is
/// written and its failure reported. The page's own `cfg` is replaced, so the
/// page itself shows the new theme immediately and `update` has something to
/// hand back. The tray — a different window on the same thread, reachable only
/// from its own `WndProc` — takes the new config the next time it feeds this
/// window a sample, which is what `handed_back` sets up.
///
/// Returns the line to show under the button. It reports what happened rather
/// than what was attempted: a write that failed says so, and nothing claims the
/// taskbar changed when it did not.
fn save_settings(hwnd: HWND, state: &mut UiState) -> String {
    let form = read_form(hwnd);
    let cfg = match form.into_config(&state.cfg) {
        Ok(cfg) => cfg,
        // Nothing is written and nothing is applied: the page keeps the values
        // the user typed so they can fix the one that was refused.
        Err(problem) => return problem,
    };

    // The file first. A config that cannot reach disk is still handed to the
    // tray below — the edit works for this session and the notice says the
    // write failed, which is the truth. The alternative, refusing the change
    // outright, would tell the user their settings did not apply when they did.
    let written = cfg.save();
    state.settings = form;
    // The page paints from this copy, so a font or colour edit is visible here
    // at once. The tray's copy is the same config, handed over by `update`.
    state.cfg = cfg;
    state.handed_back = true;

    match written {
        Ok(()) => format!("saved to {}", Config::path().display()),
        Err(e) => format!(
            "applied, but could not write {}: {e}",
            Config::path().display()
        ),
    }
}

/// Repaint the settings page after something it drew changed — a new notice, or
/// a theme colour that moved.
///
/// The fonts the window draws with belong to `UiState`, not to the controls, so
/// a font-size edit has to rebuild all three before the captions and heading
/// match the controls beside them. The list gets the new face too.
fn repaint_after_settings(hwnd: HWND, state: &mut UiState) {
    rebuild_fonts(state);
    // SAFETY: `hwnd` is our own live window.
    unsafe {
        layout(hwnd, state);
        let _ = InvalidateRect(Some(hwnd), None, true);
    }
}

/// Delete and rebuild the three faces from the current config and DPI. Shared
/// by the DPI change and the settings save, because both are "the font's inputs
/// moved" and getting one of the two paths wrong leaves stale text.
fn rebuild_fonts(state: &mut UiState) {
    // SAFETY: all three fonts are ours and are not selected into any DC between
    // paints.
    unsafe {
        for font in [&mut state.font, &mut state.bold, &mut state.title] {
            let _ = DeleteObject(HGDIOBJ(font.0));
        }
    }
    state.font = create_font(&state.cfg, state.dpi, 0, false);
    state.bold = create_font(&state.cfg, state.dpi, 1, true);
    state.title = create_font(&state.cfg, state.dpi, TITLE_EXTRA, true);

    // The brushes are the theme too: a background edit has to move them or the
    // sidebar and the controls keep the old face until the next launch.
    let bg = background(&state.cfg);
    // SAFETY: both brushes are ours; the window holds them in `UiState` and
    // replaces them here, so the old ones are no longer referenced.
    unsafe {
        let _ = DeleteObject(HGDIOBJ(state.side_brush.0));
        let _ = DeleteObject(HGDIOBJ(state.face_brush.0));
        state.side_brush = CreateSolidBrush(shade(bg, 18));
        state.face_brush = CreateSolidBrush(bg);
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
            // Ellipsised rather than clipped: an adapter description and a list
            // of resolvers can both outrun the value column, and half a word
            // looks like a rendering fault while `Realtek PCIe GbE F…` does not.
            draw(dc, state.bold, value, x0, y, x1, DT_RIGHT | DT_END_ELLIPSIS);
            y += row_h;
        }

        // The Data page's day-by-day breakdown, under the two totals above it.
        // Its rows carry runtime dates rather than the metric pages' fixed
        // captions, which is why they are not in `page_rows`.
        if page == DATA {
            for (day, bytes) in usage_rows(&state.model) {
                draw(dc, state.font, &day, x0, y, x1, DT_LEFT);
                draw(
                    dc,
                    state.bold,
                    &crate::telemetry::usage::format_size(bytes),
                    x0,
                    y,
                    x1,
                    DT_RIGHT | DT_END_ELLIPSIS,
                );
                y += row_h;
            }
        }

        // The Settings page's captions. They sit on the same bands as the
        // controls `layout_settings` places, from the same constants, so a row
        // and its caption cannot drift even though two functions draw them.
        if page == SETTINGS {
            let font = state.font;
            for (row, label) in SET_ROW_LABELS.iter().enumerate() {
                if label.is_empty() {
                    continue;
                }
                draw(dc, font, label, x0, y + row_h * row as i32, x1, DT_LEFT);
            }
            // The notice sits under the buttons rather than beside them: it is
            // the result of the whole page, not of either button, and it needs
            // the full width to name a path that did not write.
            if let Some(notice) = &state.notice {
                let colour = if notice.starts_with("saved") {
                    fg
                } else {
                    parse_color(&state.cfg.theme.alert).unwrap_or(fg_default())
                };
                SetTextColor(dc, colour);
                draw(
                    dc,
                    state.font,
                    notice,
                    x0,
                    y + row_h * (SET_ROW_COUNT as i32 + 1),
                    x1,
                    DT_LEFT,
                );
                SetTextColor(dc, fg);
            }
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

/// The theme's foreground, or a readable stand-in. Undoes the pair of
/// `unwrap_or` fallbacks that `paint` and the control colours would otherwise
/// each carry separately, so the page and the controls on it cannot disagree
/// about what colour the text is.
fn foreground(cfg: &Config) -> COLORREF {
    parse_color(&cfg.theme.foreground).unwrap_or(fg_default())
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
