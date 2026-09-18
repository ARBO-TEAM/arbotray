//! The Settings page: its form, its controls, its layout and its save.

use crate::config::{
    Config, QUOTA_MAX_GB, QUOTA_MIN_GB, clamp_font_size, clamp_interval_ms, clamp_quota_gb,
};
use crate::ui::components::{card_h, head_h};
use crate::ui::consts::{
    CTL_H, CTL_NUDGE, FIELD_W, PICK_W, SET_ALERT, SET_AUTOSTART, SET_BG,
    SET_DEFAULTS, SET_FG, SET_FONT, SET_INTERVAL, SET_OPACITY, SET_PICK_ALERT, SET_PICK_BG,
    SET_PICK_FG, SET_QUOTA, SET_QUOTA_ON, SET_RESET, SET_SAVE, SET_SPEED, SET_STOP, SET_THEME,
    SET_NOTIFY_ON, SET_NOTIFY_QUOTA, SET_NOTIFY_RATE,
    SET_TIMER_ACTION, SET_TIMER_ARM, SET_TIMER_AT, SET_TIMER_MODE, SET_TIMER_WAIT, SET_WATCH,
    SET_WATCH_RESET, SET_WIDGET, TILE_IDS, TILE_LABELS, WM_ENABLE,
};
use crate::ui::design::{CARD_PAD, CHIP, LANE_GAP};
use crate::ui::layout::{PAD, ROW_H, TITLE_EXTRA, TITLE_PAD, VALUE_OFFSET, layout, rebuild_fonts};
use crate::power;
use crate::ui::low_word;
use crate::ui::stopwatch::Stopwatch;
use crate::ui::theme::{scale, sidebar_w};
use crate::ui::UiState;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::UI::WindowsAndMessaging::{
    BS_OWNERDRAW, CreateWindowExW, ES_AUTOHSCROLL,
    ES_NUMBER, GetClientRect, GetDlgItem, GetWindowTextLengthW, GetWindowTextW, HMENU, SW_HIDE,
    SW_SHOW, SWP_NOACTIVATE, SWP_NOZORDER, SendMessageW, SetWindowPos, SetWindowTextW, ShowWindow,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_SETFONT, WS_CHILD, WS_TABSTOP,
};
use windows::core::{PCWSTR, PWSTR, w};
use std::cell::RefCell;

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
    /// Notification master switch.
    pub(crate) notify_on: bool,
    /// Plan threshold (0 – 100). Raw integer so a mid-edit blank does not
    /// become 0 and suppress the alert.
    pub(crate) notify_quota: i32,
    /// Rate alert in MB/s. `0.0` or non-finite → off.
    pub(crate) notify_rate: f64,
}

impl SettingsForm {
    /// The background as the page currently shows it.
    ///
    /// Read back from the field rather than kept beside it: the appearance row
    /// decides which preset to offer from this string, and a second copy could
    /// disagree with the box the user is looking at the moment after a Pick.
    pub(crate) fn background(&self) -> String {
        self.background.clone()
    }
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
            notify_on: cfg.notify.enabled,
            notify_quota: cfg.notify.quota_pct as i32,
            notify_rate: cfg.notify.rate_mbps,
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
        cfg.notify.enabled = self.notify_on;
        // Clamped rather than refused, like the refresh period: these are
        // bounds on a trigger, not preferences with a right answer. A `0` stays
        // reachable because it is the documented "off" value for both.
        cfg.notify.quota_pct = self.notify_quota.clamp(0, 100) as u32;
        cfg.notify.rate_mbps = if self.notify_rate.is_finite() && self.notify_rate > 0.0 {
            self.notify_rate
        } else {
            0.0
        };
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
/// The text fields keep their own state in the native controls and would lose it
/// if this were mirrored on every `WM_COMMAND`; the checkboxes cannot, because
/// they are owner-drawn and the system keeps nothing for them — `checks` is
/// where their state lives. It is only ever built while the window exists, from
/// an `hwnd` that has already been checked.
pub(crate) fn read_form(hwnd: HWND, checks: &Checks) -> SettingsForm {
    SettingsForm {
        tiles: std::array::from_fn(|i| checks.get(TILE_IDS[i])),
        // An unreadable number becomes something `into_config` refuses rather
        // than a zero it would quietly save: `0` and "not a number" have to
        // stay different.
        interval: parse_int(text_of(hwnd, SET_INTERVAL).as_deref()).unwrap_or(0),
        quota_on: checks.get(SET_QUOTA_ON),
        quota: parse_f64(text_of(hwnd, SET_QUOTA).as_deref()).unwrap_or(f64::NAN),
        font_size: parse_int(text_of(hwnd, SET_FONT).as_deref()).unwrap_or(-1),
        background: text_of(hwnd, SET_BG).unwrap_or_default(),
        foreground: text_of(hwnd, SET_FG).unwrap_or_default(),
        alert: text_of(hwnd, SET_ALERT).unwrap_or_default(),
        opacity: parse_int(text_of(hwnd, SET_OPACITY).as_deref()).unwrap_or(-1),
        widget: checks.get(SET_WIDGET),
        notify_on: checks.get(SET_NOTIFY_ON),
        // A blank field is read as `-1`, which `into_config` clamps back to 0 —
        // the documented off value. That is the right landing for a field
        // somebody is mid-edit in: quiet, not a threshold they never typed.
        notify_quota: parse_int(text_of(hwnd, SET_NOTIFY_QUOTA).as_deref()).unwrap_or(-1),
        notify_rate: parse_f64(text_of(hwnd, SET_NOTIFY_RATE).as_deref()).unwrap_or(0.0),
    }
}

/// Every owner-drawn checkbox on the window, and whether it is ticked.
///
/// The controls are `BS_OWNERDRAW`, so the system keeps no check state for them
/// — `BM_SETCHECK` is accepted and silently discarded, and `BM_GETCHECK` always
/// answers zero. This is where that state lives instead.
///
/// Keyed by control id in a `Vec` rather than held per-id in fields, because
/// the two things that have to agree about it — `read_form` reading it and the
/// `WM_DRAWITEM` handler painting it — both work from an id they were handed,
/// and a fixed struct would need a `match` in both places to reach the same
/// answer. It is a `RefCell` because the handler reaches it through a `&UiState`
/// while the click path holds a `&mut UiState`, and threading a second borrow
/// through the window proc to satisfy that would cost more than the borrow
/// flag does.
#[derive(Default)]
pub(crate) struct Checks(RefCell<Vec<(i32, bool)>>);

impl Checks {
    /// The ticked state of one checkbox, defaulting to unticked.
    ///
    /// An id that was never written reads as unticked rather than as an error:
    /// a control that failed to exist is a layout problem, not a settings one,
    /// and this is the same answer the old `BM_GETCHECK` gave for one.
    pub(crate) fn get(&self, id: i32) -> bool {
        self.0
            .borrow()
            .iter()
            .find(|(k, _)| *k == id)
            .is_some_and(|(_, v)| *v)
    }

    pub(crate) fn set(&self, id: i32, value: bool) {
        let mut all = self.0.borrow_mut();
        match all.iter_mut().find(|(k, _)| *k == id) {
            Some(slot) => slot.1 = value,
            None => all.push((id, value)),
        }
    }

    /// Flip one, and hand back the state it now holds — the auto-toggle a
    /// `BS_AUTOCHECKBOX` would have done for us.
    pub(crate) fn toggle(&self, id: i32) -> bool {
        let next = !self.get(id);
        self.set(id, next);
        next
    }
}

/// The controls the window draws as tick boxes rather than as buttons.
///
/// The `WM_DRAWITEM` handler is handed an id and nothing else, so this is the
/// one place that says which shape an id takes — and the click path reads it
/// too, because an owner-drawn box has no auto-toggle and its click has to flip
/// `Checks` by hand. That is what makes the two answers the same answer: a
/// control drawn as a box is a control that toggles.
///
/// `SET_QUOTA_ON` and `SET_WIDGET` are here as well as the tile grid, and were
/// the trap in spelling this "is a tile": they are boxes on the page, so a
/// predicate that only knew the grid would draw two of the page's checkboxes as
/// push buttons.
pub(crate) fn is_checkbox(id: i32) -> bool {
    TILE_IDS.contains(&id)
        || matches!(id, SET_QUOTA_ON | SET_AUTOSTART | SET_WIDGET | SET_NOTIFY_ON)
}

/// The edit fields — the controls that need a well painted behind them.
///
/// An `EDIT` is the one class that cannot be owner-drawn, so its rounded border
/// is drawn by the frame on the rectangle the control already occupies; this is
/// the list `paint` walks to find them. The Timer page's two are here as well as
/// the Settings page's seven, because the treatment belongs to the *control* and
/// not to the page it happens to sit on.
pub(crate) const EDIT_IDS: [i32; 11] = [
    SET_BG,
    SET_FG,
    SET_ALERT,
    SET_INTERVAL,
    SET_QUOTA,
    SET_FONT,
    SET_OPACITY,
    SET_TIMER_AT,
    SET_TIMER_WAIT,
    SET_NOTIFY_QUOTA,
    SET_NOTIFY_RATE,
];

/// Push a config into the live controls. Used to load the page and to undo a
/// failed save, so the page never shows one thing and believes another.
pub(crate) fn write_form(hwnd: HWND, checks: &Checks, cfg: &Config) {
    let form = SettingsForm::from_config(cfg);
    for (i, id) in TILE_IDS.iter().enumerate() {
        checks.set(*id, form.tiles[i]);
    }
    set_text(hwnd, SET_INTERVAL, &form.interval.to_string());
    checks.set(SET_QUOTA_ON, form.quota_on);
    set_text(hwnd, SET_QUOTA, &format_quota(form.quota));
    set_text(hwnd, SET_FONT, &form.font_size.to_string());
    set_text(hwnd, SET_BG, &form.background);
    set_text(hwnd, SET_FG, &form.foreground);
    set_text(hwnd, SET_ALERT, &form.alert);
    set_text(hwnd, SET_OPACITY, &form.opacity.to_string());
    // Not from `form`: this one is read back from the registry every time the
    // page is written, because the registry, not the config, is what Windows
    // consults — see `crate::autostart`.
    checks.set(SET_AUTOSTART, crate::autostart::enabled());
    checks.set(SET_WIDGET, form.widget);
    checks.set(SET_NOTIFY_ON, form.notify_on);
    set_text(hwnd, SET_NOTIFY_QUOTA, &form.notify_quota.to_string());
    set_text(hwnd, SET_NOTIFY_RATE, &format_quota(form.notify_rate));
    // The quota field is only meaningful while its switch is on.
    set_enabled(hwnd, SET_QUOTA, form.quota_on);
    // Both thresholds are dead while notifications are off, so the page says so
    // rather than leaving two live-looking fields that change nothing.
    set_enabled(hwnd, SET_NOTIFY_QUOTA, form.notify_on);
    set_enabled(hwnd, SET_NOTIFY_RATE, form.notify_on);
    // Last, and from the Background field just written above rather than from
    // the form: the button says which way the *page* goes, so the box beside it
    // is the only thing that can answer.
    write_theme_button(hwnd, &form.background);
}

/// The appearance button's word: the mode it switches to, not the one in force.
pub(crate) fn write_theme_button(hwnd: HWND, background: &str) {
    let (_, caption, _, _) = crate::ui::design::next_preset(background);
    set_text(hwnd, SET_THEME, caption);
}

/// The appearance button: type the other preset's two colours into the two
/// colour fields.
///
/// Staged like every field on this page — nothing is written to disk and the
/// window keeps its colours until Save — which is what makes the button safe to
/// try and safe to undo, and is why it lives here rather than on the tray menu.
/// The form's own copy moves with the boxes so the glyph under the caption
/// describes the colours the user is looking at.
pub(crate) fn apply_preset(hwnd: HWND, state: &mut UiState) -> &'static str {
    let (_, caption, bg, fg) = crate::ui::design::next_preset(&read_form(hwnd, &state.checks).background);
    set_text(hwnd, SET_BG, bg);
    set_text(hwnd, SET_FG, fg);
    set_text(hwnd, SET_THEME, caption);
    state.settings.background = bg.to_string();
    state.settings.foreground = fg.to_string();
    caption
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

/// Tick or untick one checkbox, and repaint it.
///
/// The repaint is not optional the way `BM_SETCHECK`'s was: the mark is drawn
/// by our own `WM_DRAWITEM`, so the control has no paint of its own to schedule
/// and would keep showing the old state until something else invalidated it.
pub(crate) fn set_check(state: &UiState, hwnd: HWND, id: i32, value: bool) {
    state.checks.set(id, value);
    repaint_control(hwnd, id);
}

/// Invalidate one control's rectangle, so the owner-draw handler runs again.
fn repaint_control(hwnd: HWND, id: i32) {
    // SAFETY: `ctl` is one of our own children; passing null for the rectangle
    // means "all of it", which is what a control this small wants anyway.
    unsafe {
        if let Ok(ctl) = GetDlgItem(Some(hwnd), id) {
            let _ = InvalidateRect(Some(ctl), None, false);
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
    // Owner-drawn, all three classes, so the window wears one look rather than
    // the system's. A checkbox and a push button are the same style here — the
    // `WM_DRAWITEM` handler tells them apart by id — because what separates them
    // is the shape drawn, not the control that reports the click.
    //
    // `BS_AUTOCHECKBOX` is *not* kept alongside it: `BS_` styles share one nibble
    // and cannot be combined. What it gave us is the auto-toggle, which the
    // handler does by hand instead.
    let check_style = WINDOW_STYLE(WS_CHILD.0 | (BS_OWNERDRAW as u32) | WS_TABSTOP.0);
    for (i, id) in TILE_IDS.iter().enumerate() {
        create_control(parent, state, w!("BUTTON"), TILE_LABELS[i], check_style, *id);
    }

    // `ES_LEFT` is 0 in the bindings, so it is not spelled out here.
    //
    // `WS_BORDER` is dropped: an `EDIT` cannot be owner-drawn, so the well
    // around it is painted by `WM_CTLCOLOREDIT`'s brush and the border it would
    // otherwise draw is the system's squared one. What is left is a control with
    // no edge of its own sitting inside a rounded outline `paint` draws — see
    // `FIELD_INSET`, which is the ring of that outline left visible.
    let edit_style = WINDOW_STYLE(WS_CHILD.0 | (ES_AUTOHSCROLL as u32) | WS_TABSTOP.0);
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
    create_control(parent, state, w!("BUTTON"), "", check_style, SET_NOTIFY_ON);
    // The plan threshold is a whole percentage, so `ES_NUMBER`; the rate is a
    // decimal like the plan's GB figure beside it, so it is not.
    create_control(parent, state, w!("EDIT"), "", num_style, SET_NOTIFY_QUOTA);
    create_control(parent, state, w!("EDIT"), "", edit_style, SET_NOTIFY_RATE);
    // Owner-drawn too, and the same style as the checkboxes: `WM_DRAWITEM` is
    // the only way to get a push button off the system's parts, which is why
    // Save and Reload kept their native look while the boxes did not.
    let button_style = WINDOW_STYLE(WS_CHILD.0 | (BS_OWNERDRAW as u32) | WS_TABSTOP.0);
    // One per colour row, sitting after the box it edits. Labelled for the
    // action rather than the colour, because the colour is the thing that
    // changes and a button that named it would be wrong the moment it worked.
    for (id, _) in PICK_TARGETS {
        create_control(parent, state, w!("BUTTON"), "Pick", button_style, id);
    }
    // One button for the two presets, on the row under the two colours it
    // writes. Blank here and given its word by `write_theme_button`, because the
    // caption is state — it names the mode it switches to, not the one in force.
    create_control(parent, state, w!("BUTTON"), "", button_style, SET_THEME);
    create_control(parent, state, w!("BUTTON"), "Save", button_style, SET_SAVE);
    create_control(parent, state, w!("BUTTON"), "Reload", button_style, SET_RESET);
    // The header's own button, and the one control on this page that is not on a
    // row at all: it acts on the whole form rather than editing a field, so
    // `layout_settings` pins it to the page's top-right corner and no entry of
    // `FIELD_ROWS` places it. It is created after the two above so the tab order
    // on this page ends where the page does.
    create_control(parent, state, w!("BUTTON"), "Reset to Default", button_style, SET_DEFAULTS);
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

    // The Timer page's five. The two "drop-downs" are push buttons whose caption
    // *is* the selection — see `write_timer` — so they are created with the same
    // `button_style` as everything else, and the arm button takes the foot slot
    // the other page-owned pairs use.
    create_control(parent, state, w!("BUTTON"), "At a time", button_style, SET_TIMER_MODE);
    create_control(parent, state, w!("EDIT"), "", edit_style, SET_TIMER_AT);
    create_control(parent, state, w!("EDIT"), "", num_style, SET_TIMER_WAIT);
    create_control(parent, state, w!("BUTTON"), "Sleep", button_style, SET_TIMER_ACTION);
    create_control(parent, state, w!("BUTTON"), "Arm", button_style, SET_TIMER_ARM);

    write_form(parent, &state.checks, &state.cfg);
    write_timer(parent, &state.cfg);
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
/// Rows the tile grid takes: eight checkboxes in `TILE_COLS` columns, which is
/// four. Named because card 1's height is its head plus exactly this many bands,
/// and the painter claims one band per row.
pub(crate) const TILE_ROWS: usize = 4;
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

/// Width of a row's second control — the plan's number, or Reload beside the
/// Save it undoes.
///
/// Not a field's width. The plan's number is a GB figure and Reload is a
/// six-letter word, and handing either of them a full `FIELD_W` costs `150`
/// pixels of a row whose first field has already taken `LABEL_W + FIELD_W`:
/// measured against the card's own inside at the narrowest window, that runs
/// the control `22`–`44` pixels past the plate's right edge, where it is
/// clipped rather than wrapped. At a field's width the colour row fits and
/// these two do not, which is the wrong way round — the third control is the
/// one that should give way.
pub(crate) const SECOND_W: i32 = 100;

/// Rows the second card's form occupies, numbered from zero.
///
/// They start at zero rather than continuing the page's total because the tile
/// grid above them is card 1 and carries its own numbering — `TILE_ROWS` — and
/// the only band the two ever shared was the divider, which is gone. Numbering
/// this card from zero is what lets `prefs_body_top` be the whole offset: row `n`
/// of the form sits on `prefs_body_top + n * row_h`, with no term standing for
/// the grid above it.
pub(crate) const ROW_REFRESH: usize = 0;
pub(crate) const ROW_PLAN: usize = 1;
pub(crate) const ROW_FONT: usize = 2;
pub(crate) const ROW_BG: usize = 3;
pub(crate) const ROW_FG: usize = 4;
pub(crate) const ROW_ALERT: usize = 5;
pub(crate) const ROW_OPACITY: usize = 6;
pub(crate) const ROW_STARTUP: usize = 7;
pub(crate) const ROW_WIDGET: usize = 8;
/// The three notification rows, together and directly under the plan-adjacent
/// settings they depend on: the plan threshold is meaningless without a plan,
/// and putting them apart would leave a user setting a percentage of nothing.
pub(crate) const ROW_NOTIFY: usize = 9;
pub(crate) const ROW_NOTIFY_QUOTA: usize = 10;
pub(crate) const ROW_NOTIFY_RATE: usize = 11;
/// The dark/light button, above the two colours it writes rather than beside
/// them: it is a way to *type into* the Background and Foreground fields, and a
/// row under them is where a user looks after reading the two hex strings.
pub(crate) const ROW_APPEARANCE: usize = 12;
pub(crate) const ROW_SAVE: usize = 13;
/// The rows the second card's body holds. Its height is the card head plus this
/// many bands — see `prefs_card_h`.
pub(crate) const FORM_ROWS: usize = ROW_SAVE + 1;

/// The Settings page's captions, one per row of the second card, in paint order.
///
/// Every row of this card carries a control and a caption, so not one entry is
/// blank: the tile grid is card 1 and draws nothing from this table, and the
/// rule that used to separate the grid from the form has no equivalent in a card
/// that is bounded by its own rounded edge.
///
/// This is the page's own extension point the way the card tables in `pages`
/// are for the metric pages: those return nothing here because the page has no
/// metric on it, and every line a user reads is declared in this array.
pub(crate) const SET_ROW_LABELS: [&str; FORM_ROWS] = [
    "Refresh   ms",
    "Monthly plan   GB",
    "Font size   px",
    "Background   #RRGGBB",
    "Foreground   #RRGGBB",
    "Alert   #RRGGBB",
    "Opacity   0-255",
    "Start with Windows",
    "Desktop widget",
    "Notifications",
    "Warn at plan   %",
    "Warn at rate   MB/s",
    "Appearance",
    "Write config.json",
];

/// The Timer page's captions, on bands of their own from the same `form_top`
/// the controls are placed from.
///
/// Separate from `SET_ROW_LABELS` rather than appended to it, because the two
/// pages share only their arithmetic: the Settings page's rows are card 2's,
/// numbered from zero under a grid and a card head this one has neither of, and
/// a shared array would need offsets to say which half applied.
pub(crate) const TIMER_ROW_MODE: usize = 0;
pub(crate) const TIMER_ROW_AT: usize = 1;
pub(crate) const TIMER_ROW_WAIT: usize = 2;
pub(crate) const TIMER_ROW_ACTION: usize = 3;
pub(crate) const TIMER_ROW_COUNT: usize = 4;
pub(crate) const TIMER_LABELS: [&str; TIMER_ROW_COUNT] = [
    "Trigger",
    "Time   HH:MM",
    "Wait   minutes",
    "Action",
];

/// Where each of the Timer page's controls sits. The same table shape as
/// `FIELD_ROWS`, for the same reason: the layout and the captions read one list.
pub(crate) const TIMER_FIELD_ROWS: [(i32, usize); 4] = [
    (SET_TIMER_MODE, TIMER_ROW_MODE),
    (SET_TIMER_AT, TIMER_ROW_AT),
    (SET_TIMER_WAIT, TIMER_ROW_WAIT),
    (SET_TIMER_ACTION, TIMER_ROW_ACTION),
];

/// Which row each non-tile control belongs on. One table drives both the layout
/// and the captions, so a control cannot end up under the wrong line.
pub(crate) const FIELD_ROWS: [(i32, usize); 19] = [
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
    (SET_NOTIFY_ON, ROW_NOTIFY),
    (SET_NOTIFY_QUOTA, ROW_NOTIFY_QUOTA),
    (SET_NOTIFY_RATE, ROW_NOTIFY_RATE),
    (SET_THEME, ROW_APPEARANCE),
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

// --- the two cards --------------------------------------------------------

/// Card 1's caption, and the count in its head's right slot. Both are read by
/// the painter, and the count is here rather than in `paint` because it is a
/// property of the grid this module lays out: `TILE_ROWS * TILE_COLS` checkboxes
/// are placed below it, and a head claiming a different number would be a claim
/// about a grid nobody drew.
pub(crate) const TILES_CARD_TITLE: &str = "Display Tiles";
pub(crate) const TILES_BADGE: &str = "8 tiles";
/// Card 2's caption.
pub(crate) const PREFS_CARD_TITLE: &str = "Preferences";
/// The page's own line, under the heading and above the first card. Lives here
/// rather than in `paint` because the subtitle claims a band that `cards_top`
/// counts: the painter walks the canvas and this module has to arrive at the
/// same pixel the painter's first card starts on.
pub(crate) const PAGE_SUBTITLE: &str = "Customize your ArboTray preferences";

/// A colour swatch's width at 96 DPI, painted between a colour field and the Pick
/// button beside it.
///
/// Square at 96: `SWATCH_W` and `CTL_H` are both 24, so the preview is a square
/// with air either side of it. On a DPI where they differ the swatch shrinks to
/// the control's height rather than growing taller than the row it belongs to —
/// see `swatch_rect`.
pub(crate) const SWATCH_W: i32 = 24;

/// The header's Reset to Default button, at 96 DPI. Wide enough for the two
/// words without ellipsis, and a button's width rather than a field's because it
/// is a page-wide action and must not read as a setting with a value in it.
pub(crate) const RESET_W: i32 = 132;

/// Card 1's outer height: its head plus one band per grid row.
///
/// Written as `head_h + TILE_ROWS * row_h` and not as a sum of scaled terms,
/// because this is the body the painter walks: `card` pads by `CARD_PAD` at the
/// top, `card_head` consumes `head_h`, and `TILE_ROWS` calls to `space(row_h)`
/// consume the rest. A height computed any other way would be a second answer.
pub(crate) fn tiles_card_h(dpi: u32) -> i32 {
    card_h(dpi, head_h(dpi) + TILE_ROWS as i32 * scale(ROW_H, dpi))
}

/// Card 2's outer height: its head plus one band per form row.
pub(crate) fn prefs_card_h(dpi: u32) -> i32 {
    card_h(dpi, head_h(dpi) + FORM_ROWS as i32 * scale(ROW_H, dpi))
}

/// Where the first card's top edge lands.
///
/// Summed as four scaled terms **in the painter's own walk order**, and that is
/// load-bearing rather than stylistic: `scale` truncates, so a sum of scaled
/// terms and the scaled sum differ by a pixel at some DPIs. The painter walks
/// `space(PAD)`, `space(TITLE_PAD)`, `heading(row_h + TITLE_EXTRA*2)`,
/// `subtitle` (one `row_h`), and this is that walk written out. A term out of
/// order, or one of the two folded into the other, puts every control below it a
/// pixel outside the card it is meant to be inside — which nothing else would
/// catch, because a child window drawn outside its parent's card is not an error
/// to anything.
pub(crate) fn cards_top(dpi: u32) -> i32 {
    scale(PAD, dpi)
        + scale(TITLE_PAD, dpi)
        + scale(ROW_H + TITLE_EXTRA * 2, dpi)
        + scale(ROW_H, dpi)
}

/// The band card 1's first row of checkboxes sits on: past the card's edge, its
/// padding, and its head.
pub(crate) fn tiles_body_top(dpi: u32) -> i32 {
    cards_top(dpi) + scale(CARD_PAD, dpi) + head_h(dpi)
}

/// The band card 2's first row sits on: past card 1 entirely, the lane between
/// the two cards, card 2's padding and card 2's head.
///
/// One lane gap between the cards, because that is what `Canvas::card` leaves
/// behind: it advances `top + height + LANE_GAP` when its body returns, so the
/// second card starts a lane below the first rather than hard against it.
pub(crate) fn prefs_body_top(dpi: u32) -> i32 {
    cards_top(dpi) + tiles_card_h(dpi) + scale(LANE_GAP, dpi) + scale(CARD_PAD, dpi) + head_h(dpi)
}

/// Where a card's contents start, from the content column's left edge.
///
/// A card is painted across the whole of `x0..x1` — `Canvas::card` fills that
/// rectangle and *then* indents itself — so a control at `x0` sits on the card's
/// rounded border rather than inside it.
pub(crate) fn card_inner_x0(x0: i32, dpi: u32) -> i32 {
    x0 + scale(CARD_PAD, dpi)
}

/// Where a card's contents end, from the content column's right edge.
pub(crate) fn card_inner_x1(x1: i32, dpi: u32) -> i32 {
    x1 - scale(CARD_PAD, dpi)
}

/// The colour swatch's left edge: past the field and the gap that follows it.
///
/// Its own function rather than a term in `pick_x` because two callers need it —
/// the position and the rectangle — and `swatch_x` composed with `pick_x` is the
/// whole of the colour row's arithmetic: field, gap, swatch, gap, Pick.
pub(crate) fn swatch_x(field_x: i32, field_w: i32, gap: i32) -> i32 {
    field_x + field_w + gap
}

/// A Pick button's left edge: past the swatch and a gap of its own, which is what
/// leaves the preview sitting between the box it describes and the button that
/// changes it rather than touching either.
pub(crate) fn pick_x(field_x: i32, field_w: i32, gap: i32, dpi: u32) -> i32 {
    swatch_x(field_x, field_w, gap) + scale(SWATCH_W, dpi) + gap
}

/// The colour swatch itself.
///
/// Square where the scaled numbers allow it and capped at the control's height
/// where they do not: `SWATCH_W` and `CTL_H` are both 24 at 96 DPI, but they
/// scale independently — `scale` works in whole pixels — so at some DPIs a
/// 24-pixel swatch beside a 23-pixel field would stand a pixel proud of the row
/// it is previewing. Capping it at `ctl_h` is what keeps the swatch inside its
/// band at every DPI, which is the property the tests hold it to.
///
/// The `min` can only ever choose `ctl_h`, since `SWATCH_W` is the narrower of
/// the two at every DPI this app scales for; it is written as a `min` rather than
/// as `ctl_h` so that a change to either constant cannot silently make the swatch
/// wider than the row.
pub(crate) fn swatch_rect(
    field_x: i32,
    field_w: i32,
    gap: i32,
    row_top: i32,
    ctl_h: i32,
    dpi: u32,
) -> RECT {
    let size = scale(SWATCH_W, dpi).min(ctl_h);
    let left = swatch_x(field_x, field_w, gap);
    RECT {
        left,
        top: row_top + (ctl_h - size) / 2,
        right: left + size,
        bottom: row_top + (ctl_h - size) / 2 + size,
    }
}

/// What to preview beside a colour field, or `None` on a row that carries no
/// colour.
///
/// Read from the form rather than from the box the user is looking at, because
/// the form is what a Pick writes into and what Save reads out of: a preview
/// taken from the control's text would be a third copy of a colour, and the one
/// place it could disagree with the other two.
///
/// An unparseable string is `None` rather than a fallback colour: a swatch is a
/// claim about what will be saved, and a field holding half a hex code has no
/// colour to claim. The frame the painter draws around an empty swatch is
/// therefore never drawn either — there is nothing to preview.
pub(crate) fn swatch_colour(row: usize, form: &SettingsForm) -> Option<COLORREF> {
    let hex = match row {
        ROW_BG => form.background.as_str(),
        ROW_FG => form.foreground.as_str(),
        ROW_ALERT => form.alert.as_str(),
        _ => return None,
    };
    crate::taskbar::render::parse_color(hex)
}

/// The header's Reset to Default button: `(left, top, width, height)`.
///
/// Pinned to the page's top-right rather than placed on a band, and centred
/// against the heading it sits beside: `heading` draws from the top of a band
/// `row_h + TITLE_EXTRA * 2` tall while a native button centres its own text in
/// its rectangle, so a button at the band's top edge would read as a line of the
/// heading rather than as a control. The subtraction is a half-difference of two
/// *scaled* terms, which is the painter's own band height and the scaled chip.
pub(crate) fn reset_button_rect(x1: i32, dpi: u32) -> (i32, i32, i32, i32) {
    let h = scale(CHIP, dpi);
    let w = scale(RESET_W, dpi);
    let top = scale(PAD, dpi)
        + scale(TITLE_PAD, dpi)
        + (scale(ROW_H + TITLE_EXTRA * 2, dpi) - h) / 2;
    (x1 - w, top, w, h)
}

/// How far a caption drops so it shares the centre line of the field beside it.
///
/// Measured against the controls on the running page rather than derived: a
/// native control centres its own text in its rectangle using its own font
/// metrics, while a row draws from the top of its band. Five pixels is what
/// that came to on every row of the real page — re-measure after a change to
/// the control font or to `CTL_H`.
///
/// The `field_drop(row, dpi)` helper that used to live beside this is gone with
/// the grid: it answered "does this band carry a control", and every band of the
/// second card carries one, so the answer was the same on every row that could
/// ask. The constant is still the painter's — it is applied to every row of the
/// card, unconditionally.
pub(crate) const FIELD_DROP: i32 = 5;

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

// --- the Timer page -------------------------------------------------------

/// The Timer form as the user left it: words, and one number that may not be
/// one yet.
///
/// Same shape and same reasoning as `SettingsForm`. The three words are stored
/// as the config stores them — a `power::Mode` here would have to answer what a
/// hand-edited `"at "` means before the user could see it, and the page has to
/// be able to show what is on disk rather than what this version understands.
pub(crate) struct TimerForm {
    pub(crate) mode: String,
    pub(crate) at: String,
    pub(crate) wait: i32,
    pub(crate) action: String,
}

impl TimerForm {
    /// The form for a config: the two words repaired to something the engine
    /// understands, so the page never offers a value it would then refuse.
    ///
    /// The repair is display-only. `into_config` writes back whatever the
    /// buttons say once they are clicked, and an untouched page saves the
    /// repaired word — which is the honest outcome: the page showed "At a time"
    /// and the user pressed Save, so that is what they chose.
    pub(crate) fn from_config(cfg: &Config) -> Self {
        Self {
            mode: power::mode_of(&cfg.timer).key().to_string(),
            at: if power::parse_hhmm(&cfg.timer.at).is_some() {
                cfg.timer.at.trim().to_string()
            } else {
                power::format_hhmm(
                    power::parse_hhmm(&crate::config::Timer::default().at).unwrap_or(23 * 60),
                )
            },
            wait: cfg.timer.after_min.clamp(1, TIMER_MAX_MIN as u32) as i32,
            action: power::action_of(&cfg.timer).key().to_string(),
        }
    }

    /// Apply the form to a copy of `base`.
    ///
    /// `enabled` is set here and the arm is deliberately left alone: what to fire
    /// is a setting, whether tonight is the night is not. A save therefore
    /// reconfigures a timer that is already armed without disarming it, and the
    /// armed instant a countdown is holding on to survives the edit.
    pub(crate) fn into_config(&self, base: &Config) -> Result<Config, String> {
        let minute = match power::Mode::parse(&self.mode) {
            Some(power::Mode::AtTime) => {
                let Some(m) = power::parse_hhmm(&self.at) else {
                    return Err(format!("refused: \"{}\" is not a time of day", self.at.trim()));
                };
                Some(m)
            }
            // Nothing to read: the wait is the countdown's target.
            Some(power::Mode::Countdown) => None,
            None => return Err("refused: pick a trigger".into()),
        };
        if power::Mode::parse(&self.mode) == Some(power::Mode::Countdown)
            && !(1..=TIMER_MAX_MIN).contains(&self.wait)
        {
            return Err(format!("refused: a wait of {} minutes", self.wait));
        }

        let mut cfg = base.clone();
        cfg.timer.enabled = true;
        cfg.timer.mode = self.mode.clone();
        if let Some(m) = minute {
            cfg.timer.at = power::format_hhmm(m);
        }
        cfg.timer.after_min = self.wait.max(0) as u32;
        cfg.timer.action = self.action.clone();
        Ok(cfg)
    }
}

/// The longest wait the page will accept, in minutes. A month: past that a
/// countdown is a diary entry, and the field stops bounding anything.
pub(crate) const TIMER_MAX_MIN: i32 = 60 * 24 * 31;

/// Read the Timer page's four controls.

pub(crate) fn read_timer(hwnd: HWND) -> TimerForm {
    TimerForm {
        mode: text_of(hwnd, SET_TIMER_MODE).unwrap_or_default(),
        at: text_of(hwnd, SET_TIMER_AT).unwrap_or_default(),
        wait: parse_int(text_of(hwnd, SET_TIMER_WAIT).as_deref()).unwrap_or(0),
        action: text_of(hwnd, SET_TIMER_ACTION).unwrap_or_default(),
    }
}

/// Push a config into the Timer page's controls.
///
/// The two drop-downs are push buttons whose *caption* is the selection, so
/// writing this form is `set_text` on all four — the same call the text fields
/// use. A real combo box would need a message to select an entry and another to
/// read it back, and the two would then be a second place the page could
/// disagree with itself.
pub(crate) fn write_timer(hwnd: HWND, cfg: &Config) {
    let form = TimerForm::from_config(cfg);
    let mode = power::Mode::parse(&form.mode).unwrap_or(power::Mode::AtTime);
    let action = power::Action::parse(&form.action).unwrap_or(power::Action::Sleep);
    set_text(hwnd, SET_TIMER_MODE, mode.label());
    set_text(hwnd, SET_TIMER_AT, &form.at);
    set_text(hwnd, SET_TIMER_WAIT, &form.wait.to_string());
    set_text(hwnd, SET_TIMER_ACTION, action.label());
}

/// Which of the two fields a mode actually uses, so the other can be greyed.
///
/// Both stay visible: the pair of them *is* the explanation of what the two
/// modes are, and a field that vanished on a click would move the one under it.
pub(crate) fn sync_timer_fields(hwnd: HWND, cfg: &Config) {
    let at_time = power::mode_of(&cfg.timer) == power::Mode::AtTime;
    set_enabled(hwnd, SET_TIMER_AT, at_time);
    set_enabled(hwnd, SET_TIMER_WAIT, !at_time);
}

/// Make the arm button say what the timer is doing.
///
/// The clock is the truth and the caption is read back off it, the way the
/// Stopwatch's button is — so a config edited in a text editor and a page
/// showing the wrong word cannot both be true.
pub(crate) fn sync_arm_button(hwnd: HWND, state: &UiState) {
    let label = if state.cfg.timer.enabled {
        "Disarm"
    } else {
        "Arm"
    };
    let wide: Vec<u16> = label.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: our own child, and `wide` outlives the call.
    unsafe {
        if let Ok(ctl) = GetDlgItem(Some(hwnd), SET_TIMER_ARM) {
            let _ = SetWindowTextW(ctl, PCWSTR(wide.as_ptr()));
        }
    }
}

/// Arm or disarm the timer, and return the line to show under the fields.
///
/// This is the one control on the page that is not staged behind Save, because
/// it is not a setting: it is the decision to let tonight happen, and it is
/// written to disk at once so that the tray sees it without waiting for a Save
/// that may never come. Everything above it is read first — arming a timer from
/// fields the disk does not agree with would fire something other than what the
/// page is showing.
///
/// `enabled` is the arm. `armed` carries the instant and is spent only by a
/// countdown, which has no other way to remember when it was set for; an `"at"`
/// timer recomputes its next occurrence from the clock every time it looks, so a
/// stamp there would be a second answer to a question already answered.
pub(crate) fn toggle_arm(hwnd: HWND, state: &mut UiState) -> String {
    if state.cfg.timer.enabled {
        state.cfg.timer.enabled = false;
        // The stamp goes with the arm. Left behind it would be a time the
        // engine still names while the page says the timer is off, and the
        // re-arm would then have to decide whether it was stale.
        state.cfg.timer.armed.clear();
        let written = state.cfg.save();
        state.handed_back = true;
        write_timer(hwnd, &state.cfg);
        sync_timer_fields(hwnd, &state.cfg);
        return match written {
            Ok(()) => "saved: disarmed".into(),
            Err(e) => format!("disarmed, but could not write the config: {e}"),
        };
    }

    let form = read_timer(hwnd);
    let mut cfg = match form.into_config(&state.cfg) {
        Ok(cfg) => cfg,
        Err(problem) => return problem,
    };
    let Some(when) = power::arm_target(&cfg.timer, &power::now()) else {
        return "refused: that timer names no time".into();
    };
    cfg.timer.armed = match power::mode_of(&cfg.timer) {
        power::Mode::Countdown => power::format_stamp(&when),
        power::Mode::AtTime => String::new(),
    };
    cfg.timer.enabled = true;

    let written = cfg.save();
    state.cfg = cfg;
    state.handed_back = true;
    write_timer(hwnd, &state.cfg);
    sync_timer_fields(hwnd, &state.cfg);
    let action = power::action_of(&state.cfg.timer);
    match written {
        Ok(()) => format!(
            "saved: {} at {}",
            action.label().to_lowercase(),
            power::format_stamp(&when)
        ),
        Err(e) => format!("armed, but could not write the config: {e}"),
    }
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
        let second_w = scale(SECOND_W, state.dpi);
        let nudge = scale(CTL_NUDGE, state.dpi);
        // The Timer page's own first band. That page is still a flat list of
        // rows rather than cards, so it keeps `form_top`; the Settings page
        // below measures from `cards_top` instead, which is one subtitle band
        // further down the painter's walk.
        let top = form_top(state.dpi);
        let _ = h;

        // A card is painted across the whole content column and indents itself
        // afterwards, so a control placed at `x0` sits on the card's rounded
        // border rather than inside it. Everything on the card-borne page is
        // placed from the cards' inner edges, and the tile grid — which used to
        // span `x0..x1` — now spans the card's own width.
        let ix0 = card_inner_x0(x0, state.dpi);
        let ix1 = card_inner_x1(x1, state.dpi);
        if ix1 <= ix0 {
            return;
        }

        let gap = scale(FIELD_GAP, state.dpi);
        let field_w = scale(FIELD_W, state.dpi);
        let pick_w = scale(PICK_W, state.dpi);
        // The caption column, measured from the card's own edge so that a field
        // and the caption the painter draws at the same edge keep their distance
        // on a narrow window and a wide one alike.
        let field_x = ix0 + scale(LABEL_W, state.dpi);

        // The header's own button. The one control on this page with no row in
        // `FIELD_ROWS`: it acts on the whole form rather than editing a field,
        // so it is pinned to the top-right, level with the heading it belongs
        // beside.
        let (rx, ry, rw, rh) = reset_button_rect(x1, state.dpi);
        place(hwnd, SET_DEFAULTS, rx, ry, rw, rh);

        // Card 1: the tile grid. Two columns across the card's own width and
        // `TILE_ROWS` bands of `row_h` — the same arithmetic `tiles_card_h`
        // computed the card's height from, so the card cannot be a row short of
        // the grid standing in it.
        let grid = tiles_body_top(state.dpi);
        for (i, id) in TILE_IDS.iter().enumerate() {
            let (row, col) = tile_slot(i);
            let (left, right) = tile_column(col, ix0, ix1, gap);
            place(hwnd, *id, left, grid + row_h * row as i32 + nudge, right - left, ctl_h);
        }

        // Card 2: the form. Its rows are numbered from zero — the grid above is
        // a card of its own and carries its own numbering — so a row's band is
        // the card's body top plus the row index and nothing else.
        let form = prefs_body_top(state.dpi);
        for (id, row) in FIELD_ROWS {
            let y = form + row_h * row as i32 + nudge;
            // The second and third controls on a row sit past the field that
            // owns it: the plan's switch says whether the number beside it means
            // anything, Reload sits next to the Save it undoes, and a Pick button
            // sits after the swatch that previews the colour box it edits —
            // narrow, so the three read as one row rather than as two fields
            // with a gap between them. The swatch is painted, not placed, but its
            // column is what pushes Pick out, so the two answers come from one
            // pair of functions.
            let (left, width) = if pick_button(id) {
                (pick_x(field_x, field_w, gap, state.dpi), pick_w)
            } else if right_hand_control(id) {
                (field_x + field_w + gap, second_w)
            } else {
                (field_x, field_w)
            };
            place(hwnd, id, left, y, width, ctl_h);
        }

        // The Timer page's four, on bands of their own from the same `top`, so
        // the captions the painter draws land beside the controls they name. The
        // two buttons share the field column with the two edits: a "drop-down"
        // that was any narrower than the field under it would read as a second,
        // smaller kind of thing rather than as the same setting.
        //
        // Its field column is measured from the *page's* edge and not the card's,
        // because that page has no card: the two only differ by `CARD_PAD`, and a
        // Timer control that inherited the card's indent would be the one control
        // in the window placed for a card that is not on its page.
        let timer_field_x = x0 + scale(LABEL_W, state.dpi);
        for (id, row) in TIMER_FIELD_ROWS {
            let y = top + row_h * row as i32 + nudge;
            place(hwnd, id, timer_field_x, y, field_w, ctl_h);
        }

        // The two page-owned buttons, pinned to the foot of the content column
        // rather than placed on a band. Everything above them on their own page
        // belongs to the painter, and how much of it there is depends on the
        // machine — a control on a band would end up underneath a row the
        // moment the page grew one.
        let foot = foot_button_top(h, state.dpi);
        // Two columns, one row. Every one of the five is placed on every
        // layout, so no two may share a rectangle — they belong to different
        // pages and only two are ever shown, but "shown" is not something this
        // function gets to know. The pairs are in different columns, so the
        // Speed Test and Ports buttons cannot meet the Stopwatch pair either,
        // and the Timer's lone Arm button takes the left column the Stopwatch
        // shares rather than a slot of its own.
        //
        // They used to be wrapped onto a second row, on the reasoning that four
        // 150-wide fields with a gap each is 630 against a 470-wide column at
        // the narrowest window. That was wrong twice over: only one pair is
        // shown at a time, so it is two fields — 310 — that have to fit, and a
        // second row of them starts below the client area, which is what left
        // the Stopwatch buttons hanging 14 pixels off the bottom of the window
        // with no way to see or click the lower half of either.
        for (id, column) in [
            (SET_SPEED, 0),
            (SET_STOP, 1),
            (SET_WATCH, 0),
            (SET_WATCH_RESET, 1),
            (SET_TIMER_ARM, 0),
        ] {
            let (cx, cy, cw, ch) = foot_slot(column, x0, field_w, gap, foot, ctl_h);
            place(hwnd, id, cx, cy, cw, ch);
        }
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

/// One page-owned button's rectangle on the foot row: two columns, one row.
///
/// Pure, and separate from the `place` call that consumes it, so the question
/// "does that row fit in the window" can be asked without a window to ask it
/// of. It is the question that was not being asked: the pair was wrapped onto a
/// second row, and a second row at the foot of a window that is already using
/// its last band starts *below the client area*. Both buttons were drawn 14
/// pixels off the bottom edge, and `place` reports nothing — a child window
/// positioned past its parent's edge is not an error to Windows.
pub(crate) fn foot_slot(
    column: usize,
    x0: i32,
    field_w: i32,
    gap: i32,
    foot: i32,
    ctl_h: i32,
) -> (i32, i32, i32, i32) {
    (x0 + (field_w + gap) * column as i32, foot, field_w, ctl_h)
}

/// Every control the dashboard owns, in creation order.
///
/// Named for the Settings page because that is where most of them live: this is
/// the list `layout_settings` hands the themed font to, and the one
/// `show_controls` walks. It carries the page-owned controls too, because one
/// whose font this list forgot would render in the system face — which is not a
/// failure anything else would catch.
pub(crate) fn control_ids() -> Vec<i32> {
    let mut ids: Vec<i32> = TILE_IDS.to_vec();
    ids.extend(FIELD_ROWS.iter().map(|(id, _)| *id));
    // The header button, which `FIELD_ROWS` does not carry: it is not on a row.
    // It is in this list all the same, because this list is the font sweep and
    // the visibility sweep — a control left out of it would render in the system
    // face, and would stay on screen on every other page.
    ids.push(SET_DEFAULTS);
    ids.push(SET_SPEED);
    ids.push(SET_STOP);
    ids.push(SET_WATCH);
    ids.push(SET_WATCH_RESET);
    ids.extend(TIMER_FIELD_ROWS.iter().map(|(id, _)| *id));
    ids.push(SET_TIMER_ARM);
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
    let timer = page == crate::ui::pages::TIMER;
    let elsewhere: [(i32, bool); 9] = [
        (SET_SPEED, page == crate::ui::pages::SPEEDTEST),
        (SET_STOP, page == crate::ui::pages::PORTS),
        (SET_WATCH, page == crate::ui::pages::STOPWATCH),
        (SET_WATCH_RESET, page == crate::ui::pages::STOPWATCH),
        (SET_TIMER_MODE, timer),
        (SET_TIMER_AT, timer),
        (SET_TIMER_WAIT, timer),
        (SET_TIMER_ACTION, timer),
        (SET_TIMER_ARM, timer),
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
    let form = read_form(hwnd, &state.checks);
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

// --- tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The four scale factors this window is actually built at — 100%, 125%,
    /// 150% and 200% — because `scale` truncates and a pixel of drift shows up
    /// at some of them and not others. A test at 96 alone would pass on
    /// arithmetic that is wrong at 150.
    const DPIS: [u32; 4] = [96, 120, 144, 192];

    fn row_h(dpi: u32) -> i32 {
        scale(ROW_H, dpi)
    }

    /// A form whose three colours are all `hex`, so a swatch test cannot pass by
    /// reading the wrong one of the three.
    fn form_all(hex: &str) -> SettingsForm {
        let mut f = SettingsForm::from_config(&Config::default());
        f.background = hex.into();
        f.foreground = hex.into();
        f.alert = hex.into();
        f
    }

    #[test]
    fn the_cards_top_is_the_painters_walk_summed_term_by_term() {
        for dpi in DPIS {
            let walk = scale(PAD, dpi)
                + scale(TITLE_PAD, dpi)
                + scale(ROW_H + TITLE_EXTRA * 2, dpi)
                + scale(ROW_H, dpi);
            assert_eq!(cards_top(dpi), walk, "dpi {dpi}");
        }
        // And why the terms may not be folded into one sum: at 125% the scaled
        // sum is 130 where the sum of the scaled terms is 129, a pixel of drift
        // between the painter's canvas and every control below it — a child
        // window sitting outside the card it belongs to, which Windows will draw
        // without complaint. The four DPIs above are the ones this window really
        // runs at; 96, 144 and 192 happen to agree, which is exactly why a test
        // at one of them would not have caught the fold.
        assert_ne!(
            cards_top(120),
            scale(PAD + TITLE_PAD + ROW_H + TITLE_EXTRA * 2 + ROW_H, 120),
            "the walk is a sum of scaled terms, not a scaled sum"
        );
    }

    #[test]
    fn the_two_card_bodies_are_one_card_and_a_lane_apart() {
        for dpi in DPIS {
            // The gap between the two body tops is exactly the whole of card 1
            // plus the lane `Canvas::card` leaves behind it. Nothing else may
            // creep in: a term for the subtitle or the heading here and the two
            // functions disagree about where the second card starts, which is
            // the one thing they share.
            assert_eq!(
                prefs_body_top(dpi) - tiles_body_top(dpi),
                tiles_card_h(dpi) + scale(LANE_GAP, dpi),
                "dpi {dpi}"
            );
            // Spelled out once, so that a change to either card's padding or
            // either head is a failure here rather than a row outside a card.
            assert_eq!(
                prefs_body_top(dpi) - tiles_body_top(dpi),
                scale(2 * CARD_PAD, dpi)
                    + head_h(dpi)
                    + TILE_ROWS as i32 * row_h(dpi)
                    + scale(LANE_GAP, dpi),
                "dpi {dpi}"
            );
        }
    }

    #[test]
    fn every_card_control_sits_inside_the_card_that_owns_it() {
        for dpi in DPIS {
            let ctl_h = scale(CTL_H, dpi);
            let nudge = scale(CTL_NUDGE, dpi);

            // Card 1's grid: `TILE_ROWS` bands, each one inside the card.
            let card_top = cards_top(dpi);
            let grid_bottom = card_top + tiles_card_h(dpi);
            for i in 0..TILE_IDS.len() {
                let (r, _) = tile_slot(i);
                let y = tiles_body_top(dpi) + row_h(dpi) * r as i32 + nudge;
                assert!(y >= card_top, "tile {i} above its card, dpi {dpi}");
                assert!(y + ctl_h <= grid_bottom, "tile {i} out of its card, dpi {dpi}");
            }

            // Card 2's form: every row a control is placed on, and every
            // control inside the card — including the bottom row, which is the
            // one a card a row short would cut through.
            let prefs_top = card_top + tiles_card_h(dpi) + scale(LANE_GAP, dpi);
            let prefs_bottom = prefs_top + prefs_card_h(dpi);
            for (id, row) in FIELD_ROWS {
                assert!(row < FORM_ROWS, "control {id} is off the card at dpi {dpi}");
                let band = prefs_body_top(dpi) + row_h(dpi) * row as i32;
                assert!(band >= prefs_top, "control {id} above the card, dpi {dpi}");
                assert!(
                    band + row_h(dpi) <= prefs_bottom,
                    "control {id}'s band runs out of the card, dpi {dpi}"
                );
                let y = band + nudge;
                assert!(
                    y + ctl_h <= prefs_bottom,
                    "control {id} hangs out of the card's bottom, dpi {dpi}"
                );
            }

            // Every row of the card carries a control: a band with none would
            // be a blank line in a form and, more to the point, a caption the
            // painter draws with nothing beside it.
            for row in 0..FORM_ROWS {
                assert!(
                    FIELD_ROWS.iter().any(|(_, r)| *r == row),
                    "row {row} has no control at dpi {dpi}"
                );
            }
        }
    }

    #[test]
    fn the_swatch_fits_inside_its_row_at_every_dpi() {
        for dpi in DPIS {
            let ctl_h = scale(CTL_H, dpi);
            let (field_x, field_w, gap) = (100i32, scale(FIELD_W, dpi), scale(FIELD_GAP, dpi));
            // The band top as the layout computes it: a row's band plus the
            // nudge, which is where `paint` passes `top + nudge` too.
            let row_top = prefs_body_top(dpi)
                + row_h(dpi) * ROW_BG as i32
                + scale(CTL_NUDGE, dpi);
            let r = swatch_rect(field_x, field_w, gap, row_top, ctl_h, dpi);

            // Inside its own band, top and bottom — the property that keeps the
            // preview from bleeding into the row above or below it.
            assert!(r.top >= row_top, "swatch above its band, dpi {dpi}");
            assert!(r.bottom <= row_top + ctl_h, "swatch below its band, dpi {dpi}");
            // Centred against the control it previews.
            assert_eq!(
                r.top - row_top,
                (row_top + ctl_h) - r.bottom,
                "swatch not centred in its band, dpi {dpi}"
            );
            // Square where it can be: at every DPI this app scales for, the
            // swatch is the control's own height.
            assert_eq!(r.bottom - r.top, r.right - r.left, "swatch not square, dpi {dpi}");
            assert_eq!(r.right - r.left, ctl_h, "swatch is not the band's height, dpi {dpi}");
            // And past the field it describes, so it never overlaps the box.
            assert!(r.left >= field_x + field_w, "swatch over the field, dpi {dpi}");
        }
    }

    #[test]
    fn the_pick_button_clears_the_swatch_at_every_dpi() {
        for dpi in DPIS {
            let ctl_h = scale(CTL_H, dpi);
            let (field_x, field_w, gap) = (100i32, scale(FIELD_W, dpi), scale(FIELD_GAP, dpi));
            let row_top = prefs_body_top(dpi);
            let r = swatch_rect(field_x, field_w, gap, row_top, ctl_h, dpi);
            let pick = pick_x(field_x, field_w, gap, dpi);

            // Strictly past it, with the gap the layout left: a Pick button that
            // touched or overlapped the preview would read as one control with a
            // stripe through it rather than as two.
            assert!(pick >= r.right + gap, "Pick overlaps the swatch, dpi {dpi}");
            assert_eq!(swatch_x(field_x, field_w, gap), r.left, "dpi {dpi}");
            // The row's order: field, swatch, Pick. The plan's switch and Reload
            // share a row the same way but with no preview between them, so this
            // is the only row whose geometry has three terms.
            assert!(field_x + field_w <= r.left, "dpi {dpi}");
            assert!(r.left < pick, "dpi {dpi}");
        }
    }

    #[test]
    fn the_swatch_reads_the_row_it_belongs_to() {
        let form = form_all("#FF8000");
        let orange = crate::taskbar::render::parse_color("#FF8000").unwrap();
        assert_eq!(swatch_colour(ROW_BG, &form), Some(orange));
        assert_eq!(swatch_colour(ROW_FG, &form), Some(orange));
        assert_eq!(swatch_colour(ROW_ALERT, &form), Some(orange));

        // A row that carries no colour has no preview — the swatch is drawn on
        // the three bands `swatch_rect` shares a column with, and nowhere else.
        for row in [ROW_REFRESH, ROW_FONT, ROW_OPACITY, ROW_SAVE] {
            assert_eq!(swatch_colour(row, &form), None, "row {row} previews a colour");
        }

        // Half a hex code has no colour to claim, so it claims none: a fallback
        // swatch would be a picture of a setting that is not going to be saved.
        let broken = form_all("#FF80");
        assert_eq!(swatch_colour(ROW_BG, &broken), None);
        assert_eq!(swatch_colour(ROW_FG, &broken), None);
        assert_eq!(swatch_colour(ROW_ALERT, &broken), None);
    }

    #[test]
    fn the_reset_button_sits_in_the_header_and_not_on_a_row() {
        for dpi in DPIS {
            let x1 = scale(crate::ui::layout::MIN_W, dpi) - scale(PAD, dpi);
            let (left, top, w, h) = reset_button_rect(x1, dpi);

            assert_eq!(h, scale(CHIP, dpi), "dpi {dpi}");
            assert_eq!(w, scale(RESET_W, dpi), "dpi {dpi}");
            // Flush with the content column's right edge, which is where the
            // card's edge is too.
            assert_eq!(left + w, x1, "dpi {dpi}");
            // Inside the header band: below the top pad the page opens with, and
            // above the first card's top edge.
            assert!(top >= scale(PAD, dpi), "above the page's own top, dpi {dpi}");
            assert!(top + h <= cards_top(dpi), "the button is into the cards, dpi {dpi}");
            // And it is the only control on this page with no row: it is not one
            // of the controls the form's own table places.
            assert!(
                !FIELD_ROWS.iter().any(|(id, _)| *id == SET_DEFAULTS),
                "the header button has a form row it does not use"
            );
        }
    }

    #[test]
    fn the_header_button_is_swept_with_the_page_it_belongs_to() {
        // `show_controls` hides everything not named in its own table when the
        // page is left, so a control missing from `control_ids` would never be
        // shown at all — and would keep the system font besides.
        assert!(control_ids().contains(&SET_DEFAULTS), "the header button is not swept");
        // No control may be swept twice: the list is walked once per layout and
        // once per page change.
        let ids = control_ids();
        for (i, id) in ids.iter().enumerate() {
            assert!(!ids[i + 1..].contains(id), "control {id} is in the sweep twice");
        }
        // And it is not one of the page-owned controls, which is what puts it in
        // the Settings group `show_controls` falls through to.
        for owned in [SET_SPEED, SET_STOP, SET_WATCH, SET_WATCH_RESET, SET_TIMER_ARM] {
            assert_ne!(owned, SET_DEFAULTS, "the header button belongs to another page");
        }
    }

    #[test]
    fn the_card_indents_leave_room_for_a_colour_row() {
        for dpi in DPIS {
            let x0 = scale(crate::ui::layout::SIDEBAR_W + PAD, dpi);
            let x1 = scale(crate::ui::layout::MIN_W - PAD, dpi);
            let ix0 = card_inner_x0(x0, dpi);
            let ix1 = card_inner_x1(x1, dpi);

            assert!(ix1 > ix0, "the card has no inside at dpi {dpi}");
            assert_eq!(ix0 - x0, scale(CARD_PAD, dpi), "dpi {dpi}");
            assert_eq!(x1 - ix1, scale(CARD_PAD, dpi), "dpi {dpi}");

            // The widest row on the card is a colour row, and the four
            // columns it spans have to fit inside the card's own width — at
            // the narrowest window the frame is dragged to, not only at the
            // one it opens at.
            let gap = scale(FIELD_GAP, dpi);
            let field_x = ix0 + scale(LABEL_W, dpi);
            let right = pick_x(field_x, scale(FIELD_W, dpi), gap, dpi) + scale(PICK_W, dpi);
            assert!(right <= ix1, "the colour row runs off the card, dpi {dpi}");

            // The rows the layout places — one field of `FIELD_W`, or a pair of
            // them with a gap between — have to fit inside the content column
            // at that same width. The pair is checked against the column rather
            // than the card on purpose: it is the *engine* field that is the
            // right-hand one, and it does not fit the narrower card at `MIN_W`.
            // Widening the card would mean a smaller `CARD_PAD` and a swatch
            // outside its row; the honest fix is a narrower field, and this
            // test is where that would be noticed.
            let column = x1 - x0;
            let pair = scale(LABEL_W, dpi) + scale(FIELD_W, dpi) * 2 + gap;
            assert!(pair <= column, "the plan row runs off the column, dpi {dpi}");
        }
    }

    #[test]
    fn the_card_ends_past_the_last_control_on_every_row() {
        // Every row's rightmost control has to end inside the card's own
        // inside, at the narrowest window the frame is dragged to. The three
        // shapes a row can take are checked here, built from the same
        // functions the layout places from, so this test cannot agree with
        // itself while disagreeing with the page.
        //
        // The rows that carry a second control used to hand it a full
        // `FIELD_W`, which ran the plan's number and the Reload button 22–44
        // pixels past the plate's right edge — a control clipped by the card
        // rather than wrapped, with nothing on the page to say it was there.
        for dpi in DPIS {
            let x0 = scale(crate::ui::layout::SIDEBAR_W + PAD, dpi);
            let x1 = scale(crate::ui::layout::MIN_W - PAD, dpi);
            let card_w = card_inner_x1(x1, dpi) - card_inner_x0(x0, dpi);
            let gap = scale(FIELD_GAP, dpi);
            let column = scale(LABEL_W, dpi);
            let field = scale(FIELD_W, dpi);
            let second = scale(SECOND_W, dpi);
            let field_x = column;
            let field_w = field;

            // The plain row: one field, and nothing beside it.
            assert!(column + field <= card_w, "the field row overruns, dpi {dpi}");

            // The two right-hand rows: plan's number, and Reload.
            let right_hand = column + field + gap + second;
            assert!(right_hand <= card_w, "the right-hand row overruns, dpi {dpi}");
            assert!(right_hand_control(SET_QUOTA), "the plan's number takes it, dpi {dpi}");
            assert!(right_hand_control(SET_RESET), "so does Reload, dpi {dpi}");

            // The colour row: field, preview, Pick. Three terms where the
            // right-hand rows have two, and still the narrower of the pair
            // only because Pick is a word and the second control above it is
            // a `SECOND_W` field.
            let swatch = scale(SWATCH_W, dpi);
            let pick = pick_x(field_x, field_w, gap, dpi) + scale(PICK_W, dpi);
            assert_eq!(pick, column + field + gap + swatch + gap + scale(PICK_W, dpi));
            assert!(pick <= card_w, "the colour row overruns, dpi {dpi}");
        }
    }
}
