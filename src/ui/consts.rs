//! Window class, control ids and the metrics the controls are built from.

use windows::core::{PCWSTR, w};

/// The window class, registered on first show and reused after.
pub(crate) const CLASS: PCWSTR = w!("ArboTrayWindow");

/// Where the Settings page's controls start. The sidebar is painted rather than
/// a child window, so nothing owns id 1 any more; the gap above these is kept
/// anyway, because a control added to an existing page must not renumber the
/// ones already placed on the others.
///
/// The Settings controls are addressed by id rather than by remembered handle
/// values, because hiding and showing the page is a message away and the
/// handles are read back with `GetDlgItem`. Nothing needs to keep them here.
pub(crate) const SET_ID_BASE: i32 = 10;

/// The tile checkboxes' ids, in `TILE_LABELS` order. Laying the grid out from
/// this and reading it back in the same order is what keeps the nth checkbox
/// the nth tile: there is no name-to-id lookup to get wrong.
pub(crate) const TILE_IDS: [i32; 8] = [
    SET_ID_BASE,
    SET_ID_BASE + 1,
    SET_ID_BASE + 2,
    SET_ID_BASE + 3,
    SET_ID_BASE + 4,
    SET_ID_BASE + 5,
    SET_ID_BASE + 6,
    SET_ID_BASE + 7,
];
pub(crate) const SET_INTERVAL: i32 = SET_ID_BASE + 8;
pub(crate) const SET_QUOTA_ON: i32 = SET_ID_BASE + 9;
pub(crate) const SET_QUOTA: i32 = SET_ID_BASE + 10;
pub(crate) const SET_FONT: i32 = SET_ID_BASE + 11;
pub(crate) const SET_BG: i32 = SET_ID_BASE + 12;
pub(crate) const SET_FG: i32 = SET_ID_BASE + 13;
pub(crate) const SET_ALERT: i32 = SET_ID_BASE + 14;
pub(crate) const SET_OPACITY: i32 = SET_ID_BASE + 15;
pub(crate) const SET_SAVE: i32 = SET_ID_BASE + 16;
pub(crate) const SET_RESET: i32 = SET_ID_BASE + 17;

/// The Speed Test page's one control. It is not a Settings control and is never
/// drawn with them — see `settings::show_controls` — but it shares their id
/// space, because `WM_COMMAND` reaches the window as one stream and a second
/// numbering scheme would only buy a range check.
pub(crate) const SET_SPEED: i32 = SET_ID_BASE + 18;

/// The Ports page's one control: end the process holding the selected port.
///
/// Its own id rather than a share of the Speed Test button's, and it shares the
/// Settings page's numbering for the same reason that one does — one
/// `WM_COMMAND` stream, one font sweep, one visibility function.
pub(crate) const SET_STOP: i32 = SET_ID_BASE + 19;

/// Nothing in the page is live until Save runs, so the page has to say so.
///
/// `WM_ENABLE` is the one control message the `WindowsAndMessaging` bindings
/// do not expose as a named constant, and `EnableWindow` is not there either.
/// It is a documented, stable message number, so it is spelled out rather than
/// pulling in `Win32_UI_Controls` for it.
pub(crate) const WM_ENABLE: u32 = 0x000A;

/// The button check state `BM_GETCHECK` returns for a ticked checkbox.
///
/// `BST_CHECKED` lives in `Win32_UI_Controls` with the rest of the owner-draw
/// machinery, and enabling that feature to read one `1` is not worth it. The
/// unchecked and indeterminate states are `0` and `2`; only the first two are
/// used here, and the unchecked case is `0`, so nothing spells it out.
pub(crate) const BST_CHECKED: isize = 1;

/// Text height of a settings control at 96 DPI, and the nudge that lines its
/// text up with the painted rows. A native control centres its text in its own
/// rect while the content painter draws from the top, so a control put on the
/// same band as a row sits a couple of pixels low without this.
pub(crate) const CTL_H: i32 = 24;
pub(crate) const CTL_NUDGE: i32 = 3;

/// Width of one field control — an edit box or the Save button — at 96 DPI.
/// Fixed rather than proportional: at the minimum window width the content
/// column is narrow enough that a proportional field would clip `#E6E6E6`.
pub(crate) const FIELD_W: i32 = 150;

/// The taskbar tiles' checkbox labels, in `TILE_IDS` order. They are the
/// config file's own field names, so what the page shows is what the file says.
pub(crate) const TILE_LABELS: [&str; 8] = [
    "net_down", "net_up", "latency", "cpu", "ram", "wifi", "usage", "sparkline",
];

/// The cursor left the window.
///
/// `WM_MOUSEMOVE` alone cannot tell "the pointer stopped moving over the
/// sidebar" from "the pointer left the window", so a hovered entry would stay
/// lit after the mouse was gone. This arrives exactly once per entry, after
/// `TrackMouseEvent` asks for it, which is what clears that.
///
/// `Win32_UI_Controls` declares it and `WindowsAndMessaging` does not, and the
/// feature is a much larger module than one message number is worth. It is a
/// documented, stable value, so it is spelled out — the same call as
/// `WM_ENABLE` below.
pub(crate) const WM_MOUSELEAVE: u32 = 0x02A3;

/// The up and down arrows, for moving between pages without the mouse.
///
/// A listbox navigated itself and this one does not, so the keys the list would
/// have handled are handled by the window procedure instead. The bindings put
/// these behind `Win32_UI_Input_KeyboardAndMouse`, which is enabled — for
/// `TrackMouseEvent` — but the two are plain `i32` codes here rather than a
/// `VIRTUAL_KEY` newtype, and the message handler is the only reader.
pub(crate) const VK_UP: usize = 0x26;
pub(crate) const VK_DOWN: usize = 0x28;
