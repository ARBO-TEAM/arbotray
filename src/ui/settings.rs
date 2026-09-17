//! The Settings page: its form, its controls, its layout and its save.

use crate::config::{
    Config, QUOTA_MAX_GB, QUOTA_MIN_GB, clamp_font_size, clamp_interval_ms, clamp_quota_gb,
};
use crate::ui::consts::{
    BST_CHECKED, CTL_H, CTL_NUDGE, FIELD_W, PICK_W, SET_ALERT, SET_AUTOSTART, SET_BG, SET_FG,
    SET_FONT, SET_INTERVAL, SET_OPACITY, SET_PICK_ALERT, SET_PICK_BG, SET_PICK_FG, SET_QUOTA,
    SET_QUOTA_ON, SET_RESET, SET_SAVE, SET_SPEED, SET_STOP, SET_WATCH, SET_WATCH_RESET, SET_WIDGET,
    TILE_IDS, TILE_LABELS, WM_ENABLE,
};
use crate::ui::layout::{PAD, ROW_H, TITLE_EXTRA, TITLE_PAD, VALUE_OFFSET, layout, rebuild_fonts};
use crate::ui::low_word;
use crate::ui::stopwatch::Stopwatch;
use crate::ui::theme::{scale, sidebar_w};
use crate::ui::UiState;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::UI::WindowsAndMessaging::{
    BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, BS_PUSHBUTTON, CreateWindowExW, ES_AUTOHSCROLL,
    ES_NUMBER, GetClientRect, GetDlgItem, GetWindowTextLengthW, GetWindowTextW, HMENU, SW_HIDE,
    SW_SHOW, SWP_NOACTIVATE, SWP_NOZORDER, SendMessageW, SetWindowPos, SetWindowTextW, ShowWindow,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_SETFONT, WS_BORDER, WS_CHILD, WS_TABSTOP,
};
use windows::core::{PCWSTR, PWSTR, w};

/// Everything the Settings page knows, in the form it knows it: text exactly as
/// typed, and integers parsed with a running fallback.
///
/// The page never holds a `Config` while the user is typing. Blanking one digit
/// on a numeric field is a normal thing to do, and a `Config` cannot represent
/// "no number yet" — an unparseable field would have to be either a `0` that
/// erases the setting or an error that blocks the other six. Keeping the raw
/// strings here means `into_config` is the one place a typed value becomes a
/// setting, and it is a pure function that can be tested on its own.
pub(crate) struct SettingsForm {
    /// `Show` flags, in `TILE_LABELS` order.
    tiles: [bool; 8],
    pub(crate) interval: i32,
    pub(crate) quota_on: bool,
    pub(crate) quota: f64,
    pub(crate) font_size: i32,
    background: String,
    foreground: String,
    alert: String,
    opacity: i32,
    /// Show the desktop widget. Staged like the rest of the page rather than
    /// acted on at the click, because creating the panel is window work on the
    /// tray's thread and this window cannot reach it — the save hands the
    /// config over instead, exactly as a theme edit does.
    pub(crate) widget: bool,
}

impl SettingsForm {
    /// A form showing `cfg` — what the page loads on creation.
    pub(crate) fn from_config(cfg: &Config) -> Self {
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
            widget: cfg.widget.enabled,
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
    pub(crate) fn into_config(&self, base: &Config) -> Result<Config, String> {
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
        cfg.widget.enabled = self.widget;
        Ok(cfg)
    }
}

/// Whether each tile is switched on, in `TILE_LABELS` order.
///
/// `Show` has no iterator over its eight `bool`s, so this mapping is written
/// out once here. It is exhaustive on both sides, which is what stops a field
/// being silently dropped from the page: `apply_tiles` is the inverse, and the
/// round-trip test over them fails the moment the two stop agreeing.
pub(crate) fn tile_flags(show: &crate::config::Show) -> [bool; 8] {
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
pub(crate) fn apply_tiles(show: &mut crate::config::Show, tiles: &[bool; 8]) {
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
pub(crate) fn validate_font_size(size: i32) -> Result<u32, String> {
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
pub(crate) fn validate_quota_on(on: bool, gb: f64) -> Result<Option<f64>, String> {
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
pub(crate) fn read_form(hwnd: HWND) -> SettingsForm {
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
        widget: is_checked(hwnd, SET_WIDGET),
    }
}

/// Whether one of our checkboxes is ticked. A missing control reads as unticked
/// rather than as an error: the control failing to exist is a layout problem,
/// not a settings one.
pub(crate) fn is_checked(hwnd: HWND, id: i32) -> bool {
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
pub(crate) fn write_form(hwnd: HWND, cfg: &Config) {
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
    // Not from `form`: this one is read back from the registry every time the
    // page is written, because the registry, not the config, is what Windows
    // consults — see `crate::autostart`.
    set_check(hwnd, SET_AUTOSTART, crate::autostart::enabled());
    set_check(hwnd, SET_WIDGET, form.widget);
    // The quota field is only meaningful while its switch is on.
    set_enabled(hwnd, SET_QUOTA, form.quota_on);
}

/// The quota as it appears in the edit box: one decimal for a real plan, and
/// no trailing `.0` on a whole one, because `20.0` in a field that takes a
/// decimal is a formatting choice the user would not have made.
pub(crate) fn format_quota(gb: f64) -> String {
    if gb.fract() == 0.0 {
        format!("{gb:.0}")
    } else {
        format!("{gb:.1}")
    }
}

/// A quota is typed with a decimal point, so it cannot be parsed as an integer.
pub(crate) fn parse_f64(text: Option<&str>) -> Option<f64> {
    text?.trim().parse().ok()
}

/// `#RRGGBB` for a `COLORREF`, which is `0x00BBGGRR` — the exact inverse of
/// `render::parse_color`, and the spelling the config file already holds.
pub(crate) fn format_color(c: COLORREF) -> String {
    let (r, g, b) = (c.0 & 0xFF, (c.0 >> 8) & 0xFF, (c.0 >> 16) & 0xFF);
    format!("#{r:02X}{g:02X}{b:02X}")
}

/// Open the system colour dialog on one of the colour boxes, and write the
/// answer back as `#RRGGBB`.
///
/// Seeded from whatever the box already holds, so opening the dialog and
/// pressing OK is never a way to lose a colour. A box holding something this
/// cannot read seeds from white: an unparseable colour has no better guess
/// available here, and opening on black when the page is dark would suggest the
/// dialog had read the setting rather than given up on it.
///
/// Returns whether the box was written. A dismissed dialog writes nothing —
/// including `Cancel`, which must not be a way to blank a field.
pub(crate) fn choose_color(hwnd: HWND, box_id: i32) -> bool {
    use windows::Win32::UI::Controls::Dialogs::{
        CC_FULLOPEN, CC_RGBINIT, CHOOSECOLORW, ChooseColorW,
    };

    let seeded = text_of(hwnd, box_id)
        .as_deref()
        .and_then(crate::taskbar::render::parse_color)
        .unwrap_or(COLORREF(0x00FF_FFFF));
    // The sixteen custom swatches the dialog keeps. Required to be present —
    // `lpCustColors` is not optional — and rebuilt on every call, so the dialog
    // shows its defaults rather than whatever an earlier visit happened to
    // leave, which is the same colour set every time this is opened.
    let mut custom = [COLORREF(0x00FF_FFFF); 16];
    let mut cc = CHOOSECOLORW {
        lStructSize: size_of::<CHOOSECOLORW>() as u32,
        hwndOwner: hwnd,
        rgbResult: seeded,
        lpCustColors: custom.as_mut_ptr(),
        // `CC_FULLOPEN` so the palette is there without the user having to
        // reach for "Define Custom Colors", which is the whole point of the
        // button.
        Flags: CC_RGBINIT | CC_FULLOPEN,
        ..Default::default()
    };

    // SAFETY: `cc` is a live local of the type the call expects, `hwnd` is our
    // own live window, and `custom` outlives the call — the dialog writes into
    // it and does not keep the pointer.
    let picked = unsafe { ChooseColorW(&mut cc) };
    if !picked.as_bool() {
        return false;
    }
    set_text(hwnd, box_id, &format_color(cc.rgbResult));
    true
}

/// `None` when the field holds something that is not a number, which every
/// caller treats as "unusable" rather than as zero.
pub(crate) fn parse_int(text: Option<&str>) -> Option<i32> {
    text?.trim().parse().ok()
}

/// The current text of one of our own edit controls, or `None` when the
/// control is missing or holds something that is not valid UTF-16.
pub(crate) fn text_of(hwnd: HWND, id: i32) -> Option<String> {
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
pub(crate) fn set_text(hwnd: HWND, id: i32, value: &str) {
    // SAFETY: the id names one of our own children, and `value` outlives the
    // call — `SetWindowTextW` copies it.
    unsafe {
        if let Ok(ctl) = GetDlgItem(Some(hwnd), id) {
            let mut wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
            let _ = SetWindowTextW(ctl, PWSTR(wide.as_mut_ptr()));
        }
    }
}

pub(crate) fn set_check(hwnd: HWND, id: i32, value: bool) {
    // SAFETY: `BM_SETCHECK` takes the state by value and needs no buffer.
    unsafe {
        if let Ok(ctl) = GetDlgItem(Some(hwnd), id) {
            let state = if value { BST_CHECKED } else { 0 };
            SendMessageW(ctl, BM_SETCHECK, Some(WPARAM(state as usize)), None);
        }
    }
}

/// Grey out a control that has no effect in the current state.
pub(crate) fn set_enabled(hwnd: HWND, id: i32, enabled: bool) {
    // SAFETY: `WM_ENABLE` reads its state from `wparam` and touches nothing
    // else on our own child.
    unsafe {
        if let Ok(ctl) = GetDlgItem(Some(hwnd), id) {
            SendMessageW(ctl, WM_ENABLE, Some(WPARAM(enabled as usize)), None);
        }
    }
}

/// The child id a `WM_COMMAND` carries in the low word of `wparam`.
pub(crate) fn control_id(wparam: WPARAM) -> i32 {
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
pub(crate) fn create_settings(parent: HWND, state: &mut UiState) {
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
    // Unlabelled on purpose: its painted caption sits on the same band, the way
    // every text field's does. Giving the checkbox its own text as well would
    // print the setting twice.
    create_control(parent, state, w!("BUTTON"), "", check_style, SET_AUTOSTART);
    create_control(parent, state, w!("BUTTON"), "", check_style, SET_WIDGET);
    let button_style = WINDOW_STYLE(WS_CHILD.0 | (BS_PUSHBUTTON as u32) | WS_TABSTOP.0);
    // One per colour row, sitting after the box it edits. Labelled for the
    // action rather than the colour, because the colour is the thing that
    // changes and a button that named it would be wrong the moment it worked.
    for (id, _) in PICK_TARGETS {
        create_control(parent, state, w!("BUTTON"), "Pick", button_style, id);
    }
    create_control(parent, state, w!("BUTTON"), "Save", button_style, SET_SAVE);
    create_control(parent, state, w!("BUTTON"), "Reload", button_style, SET_RESET);
    // The Speed Test page's, and the one control here that does not belong to
    // the Settings page. It shares the notification path all the same — one
    // `WM_COMMAND` stream, one `SetFont` sweep — and is separated by its own
    // function so the sweep that hides the page cannot take it with it.
    create_control(parent, state, w!("BUTTON"), "Run test", button_style, SET_SPEED);
    // The Ports page's, and the second control that belongs to another page.
    // Same path, same separation: `show_controls` is the one place the split is
    // spelled out.
    create_control(parent, state, w!("BUTTON"), "Stop process", button_style, SET_STOP);
    // The Stopwatch page's two, and the first controls here whose *text* is
    // state rather than a fixed caption: the first says Start or Stop and the
    // second is a caption. `sync_watch_button` is what keeps the first one
    // honest, and it is called from the same places the clock is.
    create_control(parent, state, w!("BUTTON"), "Start", button_style, SET_WATCH);
    create_control(parent, state, w!("BUTTON"), "Reset", button_style, SET_WATCH_RESET);

    write_form(parent, &state.cfg);
}

/// One child control, hidden until its page is showing.
pub(crate) fn create_control(
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
pub(crate) const TILE_COLS: usize = 2;
/// Gap between the tile columns, and between a caption's field and the value
/// column beside it.
pub(crate) const FIELD_GAP: i32 = 10;

/// The caption column: how far in from the content edge a field control starts.
///
/// Load-bearing rather than cosmetic. `paint` draws each caption at the content
/// edge and the frame carries `WS_CLIPCHILDREN`, so a control placed at that
/// same edge is painted over its own label — it is clipped, not truncated, and
/// the page then shows a column of boxes with nothing to say what they are.
/// Every caption in `SET_ROW_LABELS` is written to fit this width.
pub(crate) const LABEL_W: i32 = 150;

/// Rows the whole settings page occupies. The tile grid takes the first four,
/// a divider takes the fifth, and every labelled field under it takes one more.
///
/// The divider's own row is the one band on this page that is neither a control
/// nor a caption, which is why it is named: the grid above it and the form below
/// it are two different kinds of thing, and the rule is the only mark on the
/// page that says so.
pub(crate) const ROW_DIVIDER: usize = 4;
pub(crate) const ROW_REFRESH: usize = 5;
pub(crate) const ROW_PLAN: usize = 6;
pub(crate) const ROW_FONT: usize = 7;
pub(crate) const ROW_BG: usize = 8;
pub(crate) const ROW_FG: usize = 9;
pub(crate) const ROW_ALERT: usize = 10;
pub(crate) const ROW_OPACITY: usize = 11;
pub(crate) const ROW_STARTUP: usize = 12;
pub(crate) const ROW_WIDGET: usize = 13;
pub(crate) const ROW_SAVE: usize = 14;
pub(crate) const SET_ROW_COUNT: usize = ROW_SAVE + 1;

/// The Settings page's captions, one per row in paint order, with whatever a
/// bare number would not say for itself. The four rows the tile grid occupies
/// and the divider's row have no caption of their own — a checkbox carries its
/// own label, and a rule carries none — so they are empty strings rather than a
/// shorter array with an offset to get wrong.
///
/// This is the page's own extension point, the way `page_rows` is for the
/// metric pages: `page_rows` returns nothing here because the page has no
/// metric on it, and every line a user reads is declared in this array.
pub(crate) const SET_ROW_LABELS: [&str; SET_ROW_COUNT] = [
    "",
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
    "Start with Windows",
    "Desktop widget",
    "Write config.json",
];

/// Which row each non-tile control belongs on. One table drives both the layout
/// and the captions, so a control cannot end up under the wrong line.
pub(crate) const FIELD_ROWS: [(i32, usize); 15] = [
    (SET_INTERVAL, ROW_REFRESH),
    (SET_QUOTA_ON, ROW_PLAN),
    (SET_QUOTA, ROW_PLAN),
    (SET_FONT, ROW_FONT),
    (SET_BG, ROW_BG),
    (SET_FG, ROW_FG),
    (SET_ALERT, ROW_ALERT),
    (SET_OPACITY, ROW_OPACITY),
    (SET_AUTOSTART, ROW_STARTUP),
    (SET_WIDGET, ROW_WIDGET),
    (SET_SAVE, ROW_SAVE),
    (SET_RESET, ROW_SAVE),
    (SET_PICK_BG, ROW_BG),
    (SET_PICK_FG, ROW_FG),
    (SET_PICK_ALERT, ROW_ALERT),
];

/// The controls that sit in the value column rather than at the field's own
/// left edge — a second control sharing a row with the first. Save and Reload
/// are the pair; the plan's switch and number are the other.
pub(crate) fn right_hand_control(id: i32) -> bool {
    matches!(id, SET_RESET | SET_QUOTA)
}

/// How far a caption drops so it shares the centre line of the field beside it.
///
/// Measured against the controls on the running page rather than derived: a
/// native control centres its own text in its rectangle using its own font
/// metrics, while a row draws from the top of its band. Five pixels is what
/// that came to on every row of the real page — re-measure after a change to
/// the control font or to `CTL_H`.
pub(crate) const FIELD_DROP: i32 = 5;

/// The drop for `row`, and zero on the bands that carry no control — the
/// divider claims one, and moving it would slide the rule off its own band.
pub(crate) fn field_drop(row: usize, dpi: u32) -> i32 {
    if FIELD_ROWS.iter().any(|(_, r)| *r == row) {
        scale(FIELD_DROP, dpi)
    } else {
        0
    }
}

/// Whether a control is one of the colour fields' Pick buttons, which take a
/// word's width rather than a field's. Kept apart from `right_hand_control`
/// because the two answer different questions — that one says *which side of
/// the row*, this one says *how much of it* — and a button that is both would
/// have to be right for the wrong reason.
pub(crate) fn pick_button(id: i32) -> bool {
    matches!(id, SET_PICK_BG | SET_PICK_FG | SET_PICK_ALERT)
}

/// The colour field a Pick button edits: `(button, box, which colour)`. One
/// table, read by both the click handler and the tests, so a button wired to
/// the wrong box is a row that fails rather than a dialog that quietly edits
/// the foreground when the user asked for the alert colour.
pub(crate) const PICK_TARGETS: [(i32, i32); 3] = [
    (SET_PICK_BG, SET_BG),
    (SET_PICK_FG, SET_FG),
    (SET_PICK_ALERT, SET_ALERT),
];

/// The edit box one of the colour dialogs writes back to.
pub(crate) fn pick_target(button: i32) -> Option<i32> {
    PICK_TARGETS
        .iter()
        .find(|(pick, _)| *pick == button)
        .map(|(_, box_id)| *box_id)
}

/// Which grid slot a tile's checkbox takes: the row under row 0, and the
/// column within the field. Pure, so the packing can be checked without a
/// window — eight tiles in two columns is four rows only if this agrees.
pub(crate) fn tile_slot(index: usize) -> (usize, usize) {
    (index / TILE_COLS, index % TILE_COLS)
}

/// Where a tile checkbox sits: the left and right edges of column `col` inside
/// a field `x0..x1` wide, with `gap` already scaled.
pub(crate) fn tile_column(col: usize, x0: i32, x1: i32, gap: i32) -> (i32, i32) {
    let span = ((x1 - x0) - gap * (TILE_COLS as i32 - 1)) / TILE_COLS as i32;
    let left = x0 + (span + gap) * col as i32;
    (left, left + span)
}

/// Put the settings controls on the rows `SET_ROW_LABELS` names, and give them
/// the window's font so they match the painted text beside them.
///
/// The controls are laid out even while hidden: they are hidden with
/// `SW_HIDE`, which leaves a control's rectangle alone, so the next time the
/// page is shown it is already correct — including after a resize or a DPI
/// change that happened while it was off screen.
pub(crate) fn layout_settings(hwnd: HWND, state: &mut UiState) {
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
        // Unconditional now that the sidebar is painted rather than a child
        // window: there is no "the list failed to create" case left, and the
        // controls therefore always line up under the captions `paint`draws.
        let x0 = side + pad;
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
        let top = form_top(state.dpi);
        let _ = h;

        let field_w = scale(FIELD_W, state.dpi);
        let pick_w = scale(PICK_W, state.dpi);
        let gap = scale(FIELD_GAP, state.dpi);
        let field_x = x0 + scale(LABEL_W, state.dpi);

        for (i, id) in TILE_IDS.iter().enumerate() {
            let (row, col) = tile_slot(i);
            let (left, right) = tile_column(col, x0, x1, gap);
            place(hwnd, *id, left, top + row_h * row as i32 + nudge, right - left, ctl_h);
        }

        for (id, row) in FIELD_ROWS {
            let y = top + row_h * row as i32 + nudge;
            // The second and third controls on a row sit past the field that
            // owns it: the plan's switch says whether the number beside it means
            // anything, Reload sits next to the Save it undoes, and a Pick button
            // sits after the colour box it edits — narrow, so the three read as
            // one row rather than as two fields with a gap between them.
            let (left, width) = if pick_button(id) {
                (field_x + field_w + gap, pick_w)
            } else if right_hand_control(id) {
                (field_x + field_w + gap, field_w)
            } else {
                (field_x, field_w)
            };
            place(hwnd, id, left, y, width, ctl_h);
        }

        // The two page-owned buttons, pinned to the foot of the content column
        // rather than placed on a band. Everything above them on their own page
        // belongs to the painter, and how much of it there is depends on the
        // machine — a control on a band would end up underneath a row the
        // moment the page grew one.
        let foot = foot_button_top(h, state.dpi);
        // Two columns, two rows. Every one of the four is placed on every
        // layout, so no two may share a rectangle — they belong to different
        // pages and only two are ever shown, but "shown" is not something this
        // function gets to know.
        //
        // Wrapped rather than run along one line because the content column is
        // only `MIN_W - SIDEBAR_W - 2 * PAD` = 470 wide at the narrowest window
        // and its floor: four 150-wide fields with a gap each is 630, which
        // would push the last two off the edge of the column entirely.
        place(hwnd, SET_SPEED, x0, foot, field_w, ctl_h);
        place(hwnd, SET_STOP, x0 + field_w + gap, foot, field_w, ctl_h);
        place(hwnd, SET_WATCH, x0, foot + ctl_h + gap, field_w, ctl_h);
        place(hwnd, SET_WATCH_RESET, x0 + field_w + gap, foot + ctl_h + gap, field_w, ctl_h);
        // A second run while one is in flight is refused by `SpeedTest::start`
        // anyway; disabling it here is what says so before the click rather
        // than after it. Refreshed per tick by `ui::update`, because the run
        // that re-enables it ends on another thread.
        set_enabled(hwnd, SET_SPEED, !state.model.speed_running);
        // The same for the Stop button: nothing selected, nothing to stop. Its
        // selection lives in `UiState` and can be cleared between two samples —
        // by a click, or by the port simply closing — so this is refreshed per
        // tick as well rather than only when the selection changes.
        set_enabled(hwnd, SET_STOP, state.selected_port.is_some());

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

/// The band the form's first row sits on: one title-height below the page
/// heading, which is exactly where `paint` puts its own first row, so a caption
/// and the control beside it share a band.
///
/// Summed as three scaled terms rather than one scaled sum, and that is not
/// incidental: `scale` truncates, so `scale(20) + scale(54) + scale(6)` and
/// `scale(80)` differ by a pixel at some DPIs — a pixel of drift between a
/// caption and its control, which is the one thing this function exists to
/// prevent.
pub(crate) fn form_top(dpi: u32) -> i32 {
    scale(PAD, dpi)
        + scale(TITLE_PAD + ROW_H + TITLE_EXTRA * 2, dpi)
        + scale(VALUE_OFFSET, dpi)
}

/// Where a page's own action button sits: the foot of the content column, but
/// never above the first band — a window shorter than its own content scrolls
/// nothing, so the button would otherwise be drawn over the heading.
///
/// Shared by the two pages that own a control of their own, and exposed because
/// their painters have to know it too. Both lists above these buttons are
/// unbounded in a way no other page's content is — ten speed runs, twelve
/// listening ports — and a list that grew into its button would leave the
/// button unclickable rather than merely ugly. Two copies of this arithmetic
/// would drift; one function is the only way a painter and its control agree on
/// where the page ends.
pub(crate) fn foot_button_top(h: i32, dpi: u32) -> i32 {
    (h - scale(PAD, dpi) - scale(CTL_H, dpi)).max(form_top(dpi))
}

/// Every control the dashboard owns, in creation order.
///
/// Named for the Settings page because that is where all but one of them live:
/// this is the list `layout_settings` hands the themed font to, and the one
/// `show_controls` walks. It carries `SET_SPEED` too, because a control whose
/// font this list forgot would render in the system face — which is not a
/// failure anything else would catch.
pub(crate) fn control_ids() -> Vec<i32> {
    let mut ids: Vec<i32> = TILE_IDS.to_vec();
    ids.extend(FIELD_ROWS.iter().map(|(id, _)| *id));
    ids.push(SET_SPEED);
    ids.push(SET_STOP);
    ids.push(SET_WATCH);
    ids.push(SET_WATCH_RESET);
    ids
}

/// Move one control into place. `SWP_SHOWWINDOW` is deliberately absent: which
/// page is showing is `show_settings`'s business, not the layout's.
pub(crate) fn place(hwnd: HWND, id: i32, x: i32, y: i32, w: i32, h: i32) {
    // SAFETY: `set_enabled`-style lookup of one of our own children; the call
    // only moves that child.
    unsafe {
        if let Ok(ctl) = GetDlgItem(Some(hwnd), id) {
            let _ = SetWindowPos(ctl, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
}

/// Show the page's one control, or hide it. Separate from the sweep below
/// because the two changes at different times: the sweep is the Settings page
/// being left, this is the Speed Test page being reached.
///
/// It exists at all because *every* control is a real child window and none of
/// them is clipped to a page — `WS_CLIPCHILDREN` clips children to their own
/// rectangles, not to a region the parent paints. So a control that is only
/// hidden when its neighbour's page is left would sit on top of the dashboard
/// for as long as the user stayed there.
pub(crate) fn show_controls(hwnd: HWND, page: usize) {
    let settings = page == crate::ui::pages::SETTINGS;
    // The controls that belong to a page of their own, and the page each one
    // belongs to. A table rather than a chain of `if`s: this is the one place
    // the split between "the Settings page's controls" and "the rest" is
    // written down, and a third page-owned control should be a row here rather
    // than another branch in two places.
    let elsewhere: [(i32, bool); 4] = [
        (SET_SPEED, page == crate::ui::pages::SPEEDTEST),
        (SET_STOP, page == crate::ui::pages::PORTS),
        (SET_WATCH, page == crate::ui::pages::STOPWATCH),
        (SET_WATCH_RESET, page == crate::ui::pages::STOPWATCH),
    ];
    // SAFETY: every id names one of our own children; `ShowWindow` on a child
    // only changes its visibility.
    unsafe {
        for id in control_ids() {
            let owned = elsewhere.iter().find(|(own, _)| *own == id);
            let show = match owned {
                Some((_, visible)) => *visible,
                None => settings,
            };
            if let Ok(ctl) = GetDlgItem(Some(hwnd), id) {
                let _ = ShowWindow(ctl, if show { SW_SHOW } else { SW_HIDE });
            }
        }
    }
}

/// Make the Stopwatch page's first button say what the clock is doing.
///
/// The one control in this window whose caption is state rather than a fixed
/// label. It reads the clock rather than being handed a flag, so there is no
/// way to pass the wrong one: the caption is derived from the same value the
/// page paints, and a second copy of "is it running" cannot come apart from it.
pub(crate) fn sync_watch_button(hwnd: HWND, state: &UiState) {
    let label = Stopwatch::label(state.watch.is_running());
    let wide: Vec<u16> = label.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `hwnd` is our own window, `wide` outlives the call, and the id
    // names one of our own children.
    unsafe {
        if let Ok(ctl) = GetDlgItem(Some(hwnd), SET_WATCH) {
            let _ = SetWindowTextW(ctl, PCWSTR(wide.as_ptr()));
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
pub(crate) fn save_settings(hwnd: HWND, state: &mut UiState) -> String {
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

pub(crate) fn repaint_after_settings(hwnd: HWND, state: &mut UiState) {
    rebuild_fonts(state);
    // SAFETY: `hwnd` is our own live window.
    unsafe {
        layout(hwnd, state);
        let _ = InvalidateRect(Some(hwnd), None, true);
    }
}
