//! The page list down the left, drawn by hand.
//!
//! This used to be a native `LISTBOX`, which brought selection, keyboard
//! navigation and scrolling for free. It was replaced because a system list
//! paints its own items, and an entry therefore cannot carry a glyph — five
//! bare words in the one column that is supposed to read as a sidebar is the
//! part of the window that looked unfinished.
//!
//! The alternative was `LBS_OWNERDRAWFIXED`, and it is the reason this is drawn
//! by the parent instead. Owner-draw would have pulled in `Win32_UI_Controls`
//! for the same result, it still hands the selection pill to the system's own
//! `DRAWITEMSTRUCT`, and it keeps the list's grey scroll bar in the corner of a
//! themed window. Drawing it here is fewer moving parts than bending the system
//! control, and the window paints its sidebar in the same pass as its content —
//! one DC, so the two can never land a frame apart.
//!
//! What the listbox gave away for free had to be written back: hit testing,
//! hover, and `WM_KEYDOWN`. See `item_at`, and the mouse and keyboard arms of
//! the window procedure in `mod.rs`.

use crate::ui::components::{draw, glyph, rounded_fill};
use crate::ui::design::{ICON_COL, ITEM_H, RADIUS, S2, S3, page_icon, palette};
use crate::ui::layout::{ROW_H, VALUE_OFFSET};
use crate::ui::pages::PAGES;
use crate::ui::settings::{show_controls, sync_arm_button, sync_timer_fields};
use crate::ui::theme::{scale, sidebar_w};
use crate::ui::UiState;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{DT_LEFT, HDC, HFONT, InvalidateRect, SetTextColor};

/// The gap above the first entry. Small: the sidebar has no header of its own,
/// and the page's own heading on the other side of the divider is what the eye
/// should land on first.
///
/// Visible to the layout, which derives the window's minimum height from the
/// list's own geometry rather than from a number kept in step by hand.
pub(crate) const TOP: i32 = S3;

/// How far the selection pill is inset from the sidebar's edges, and the sliver
/// of air above and below its label.
const PILL_INSET: i32 = S2;
const PILL_V: i32 = 2;

/// Which entry is under `(x, y)`, or `None` for the padding around them.
///
/// Takes plain numbers rather than the state and the window so the geometry can
/// be tested without either: an off-by-one here is a click that selects its
/// neighbour, which reads as lag rather than as a bug.
fn item_at(x: i32, y: i32, side: i32, dpi: u32) -> Option<usize> {
    if x < 0 || x >= side {
        return None;
    }
    let top = scale(TOP, dpi);
    if y < top {
        return None;
    }
    let index = ((y - top) / scale(ITEM_H, dpi)) as usize;
    // Below the last entry is empty space, not a clipped last entry.
    if index < PAGES.len() { Some(index) } else { None }
}

/// Which entry is under the cursor, in client coordinates.
pub(crate) fn hit_test(state: &UiState, x: i32, y: i32) -> Option<usize> {
    item_at(x, y, sidebar_w(state.dpi), state.dpi)
}

/// Show `page`, from a click or from a key.
///
/// The one place a page ever changes, so the settings controls are shown and
/// hidden on the same path whatever asked for them. They are clipped to their
/// own rectangles and not to the page that owns them, so leaving them up would
/// paint eight checkboxes across the sparkline.
pub(crate) fn select(hwnd: HWND, state: &mut UiState, page: usize) {
    let page = page.min(PAGES.len() - 1);
    if page == state.page {
        return;
    }
    state.page = page;
    show_controls(hwnd, page);
    // The Timer page's two state-bearing captions — the arm button's verb and
    // which of its two fields is live. Refreshed here rather than only where the
    // config changes, because these are the controls that decide whether they
    // are shown at all: arriving on the page is the only moment either is read.
    if page == crate::ui::pages::TIMER {
        sync_arm_button(hwnd, state);
        sync_timer_fields(hwnd, &state.cfg);
    }
    // SAFETY: our own window, and an invalidation only asks for a repaint.
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

/// Paint the sidebar: its face, its divider, and one entry per page.
///
/// # Safety
/// `dc` must be a live DC, and it must be the window's own — the face fills the
/// region the content column was drawn in as well, so this has to run while the
/// DC still holds the frame.
pub(crate) unsafe fn paint(dc: HDC, client: &RECT, state: &UiState) {
    let pal = palette(&state.cfg);
    let side = sidebar_w(state.dpi);
    let height = client.bottom - client.top;

    // SAFETY: the caller guarantees a live DC.
    unsafe {
        rounded_fill(
            dc,
            RECT {
                left: 0,
                top: 0,
                right: side,
                bottom: height,
            },
            0,
            pal.sidebar,
        );
        // A one-pixel-wide fill rather than a stroked line: it needs no pen
        // selected into the DC and nothing to select back out, which is the
        // whole reason `rounded_fill` installs the null pen.
        rounded_fill(
            dc,
            RECT {
                left: side - 1,
                top: 0,
                right: side,
                bottom: height,
            },
            0,
            pal.border,
        );
    }

    let item_h = scale(ITEM_H, state.dpi);
    let top = scale(TOP, state.dpi);
    let inset = scale(PILL_INSET, state.dpi);
    let pill_v = scale(PILL_V, state.dpi);
    let icon_col = scale(ICON_COL, state.dpi);
    let label_x = inset + scale(S2, state.dpi);
    let icon_font = crate::ui::design::icon_font(&state.cfg, state.dpi);
    // The label sits where the content area's rows sit inside their own band,
    // from the same two constants, so the two columns of text look level.
    let nudge = (item_h - scale(ROW_H, state.dpi)) / 2 + scale(VALUE_OFFSET, state.dpi);

    for (index, name) in PAGES.iter().enumerate() {
        let y = top + item_h * index as i32;
        let selected = index == state.page;
        let hovered = state.hover == Some(index);

        if selected || hovered {
            // Filled the same way either way, so the cursor crossing the list
            // does not change the pill's shape — only its colour.
            let fill = if selected { pal.accent } else { pal.selected };
            // SAFETY: a live DC, as above.
            unsafe {
                rounded_fill(
                    dc,
                    RECT {
                        left: inset,
                        top: y + pill_v,
                        right: side - inset,
                        bottom: y + item_h - pill_v,
                    },
                    RADIUS,
                    fill,
                );
            }
        }

        // Label and glyph share the entry's colour, so a selected row is one
        // object rather than a blue pill with a stray grey icon in it.
        let colour = if selected {
            pal.accent_text
        } else if hovered {
            pal.text
        } else {
            pal.muted
        };
        let font: HFONT = if selected { state.bold } else { state.font };

        // SAFETY: a live DC; both faces belong to `UiState` and outlive the
        // window, and `draw` restores nothing the next entry depends on.
        unsafe {
            SetTextColor(dc, colour);
            glyph(dc, icon_font, page_icon(index), label_x, y + nudge);
            draw(dc, font, name, label_x + icon_col, y + nudge, side - inset, DT_LEFT);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 96 DPI, where a scaled constant is the constant.
    const DPI: u32 = 96;

    fn band(index: usize) -> i32 {
        scale(TOP, DPI) + scale(ITEM_H, DPI) * index as i32
    }

    #[test]
    fn a_click_lands_on_the_entry_it_looks_like() {
        let side = sidebar_w(DPI);
        for index in 0..PAGES.len() {
            let y = band(index);
            // The top edge, the middle and the last pixel all belong to the
            // entry: a target that only answers in its centre is a target the
            // user misses.
            for probe in [y, y + scale(ITEM_H, DPI) / 2, y + scale(ITEM_H, DPI) - 1] {
                assert_eq!(
                    item_at(side / 2, probe, side, DPI),
                    Some(index),
                    "entry {index} at y={probe}"
                );
            }
        }
    }

    #[test]
    fn the_padding_and_the_outside_are_not_entries() {
        let side = sidebar_w(DPI);
        // Above the first entry, and below the last.
        assert_eq!(item_at(side / 2, scale(TOP, DPI) - 1, side, DPI), None);
        assert_eq!(item_at(side / 2, band(PAGES.len()), side, DPI), None);
        // Across the divider, and off the left edge.
        assert_eq!(item_at(side, band(0), side, DPI), None);
        assert_eq!(item_at(-1, band(0), side, DPI), None);
    }

    #[test]
    fn entries_scale_with_the_display_and_never_overlap() {
        // At every scale the last pixel of one entry and the first of the next
        // are different entries, and the last entry is still inside the width.
        for dpi in [96u32, 120, 144, 192] {
            let side = sidebar_w(dpi);
            let item_h = scale(ITEM_H, dpi);
            for index in 0..PAGES.len() {
                let y = scale(TOP, dpi) + item_h * index as i32;
                assert_eq!(
                    item_at(side / 2, y, side, dpi),
                    Some(index),
                    "dpi {dpi}, entry {index}"
                );
                assert_eq!(
                    item_at(side / 2, y + item_h - 1, side, dpi),
                    Some(index),
                    "dpi {dpi}, entry {index} last pixel"
                );
            }
        }
    }
}
