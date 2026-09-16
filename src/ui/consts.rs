//! Window class, control ids and the metrics the controls are built from.

use windows::core::{PCWSTR, w};

pub(crate) const CLASS: PCWSTR = w!("ArboTrayWindow");

/// Child id of the page list, handed to `CreateWindowExW` as an `HMENU` and
/// read back out of the low word of `WM_COMMAND`'s `wparam`.
pub(crate) const LIST_ID: i32 = 1;

/// Where the Settings page's controls start. Deliberately far above `LIST_ID`:
/// the list owns 1 and nothing else may take it, and a gap leaves room for
/// another control on an existing page without renumbering anything.
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
