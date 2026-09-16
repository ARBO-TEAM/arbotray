//! 96-DPI metrics and the font rebuild.

use crate::ui::fonts::create_font;
use crate::ui::settings::layout_settings;
use crate::ui::theme::background;
use crate::ui::UiState;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{CreateSolidBrush, DeleteObject, HGDIOBJ};

/// Metrics in 96-DPI pixels. The font and the sidebar width carry the scaling;
/// these are the ratios everything else is laid out from.
pub(crate) const PAD: i32 = 20;
pub(crate) const ROW_H: i32 = 30;
pub(crate) const VALUE_OFFSET: i32 = 6;
pub(crate) const SPARK_GAP: i32 = 14;
pub(crate) const SIDEBAR_W: i32 = 150;
pub(crate) const TITLE_EXTRA: i32 = 6;

/// Points above the configured size for the Stopwatch page's clock.
///
/// Far enough above the body that the reading is legible from across a desk,
/// because on that page it is the *only* thing there is to read. A band would
/// not do it: the rows are 30 pixels tall and a face that filled one would still
/// be body-sized.
pub(crate) const CLOCK_EXTRA: i32 = 36;

/// Air above the page title.
///
/// The first band on a page is laid out from its top, so a title larger than a
/// row would start at the very edge of the window with all its extra height
/// below it. Padding the top instead moves the whole block — title and rows —
/// down together, which is the difference between a page and a screenshot
/// pinned to the ceiling. The Settings page's controls are off by the same
/// amount, so it has to be a constant rather than a number in one painter.
pub(crate) const TITLE_PAD: i32 = 12;

/// Smallest size still showing every row without the frame collapsing.
///
/// Two columns' worth of furniture before any content: the sidebar eats its
/// share, and since the pages are grouped, every row's label starts a group
/// heading column in from the content edge. At the old 520 the widest pair in
/// the window — `Processor` beside `AMD Ryzen 5 7600 6-Core Processor` — no
/// longer fit and would have been ellipsised, so the floor moved out with them.
pub(crate) const MIN_W: i32 = 660;
pub(crate) const MIN_H: i32 = 320;

/// Initial size: room for the rows plus a decent sparkline.
///
/// `START_H` tracks the Settings page's row count — the tallest page, since it
/// is the one that grows a band per setting. The last band starts at
/// `form_top(96) + ROW_H * ROW_SAVE` = 470, so the floor is that plus a whole
/// row of margin; a page that grows a row without this moving would open with
/// its last field under the frame's edge, and nothing in the layout would
/// complain.
pub(crate) const START_W: i32 = 720;
pub(crate) const START_H: i32 = 520;

/// Put the Settings controls where they belong. Called on every resize, which
/// is also what keeps them correct across a DPI change.
///
/// The sidebar used to be placed from here too, as a child window with its own
/// rectangle. It is painted now, so its geometry comes from the same constants
/// the painter uses and there is nothing to position.
pub(crate) fn layout(hwnd: HWND, state: &mut UiState) {
    layout_settings(hwnd, state);
}

/// Delete and rebuild the three faces from the current config and DPI. Shared
/// by the DPI change and the settings save, because both are "the font's inputs
/// moved" and getting one of the two paths wrong leaves stale text.
pub(crate) fn rebuild_fonts(state: &mut UiState) {
    // SAFETY: all four fonts are ours and are not selected into any DC between
    // paints.
    unsafe {
        for font in [&mut state.font, &mut state.bold, &mut state.title, &mut state.clock] {
            let _ = DeleteObject(HGDIOBJ(font.0));
        }
    }
    state.font = create_font(&state.cfg, state.dpi, 0, false);
    state.bold = create_font(&state.cfg, state.dpi, 1, true);
    state.title = create_font(&state.cfg, state.dpi, TITLE_EXTRA, true);
    state.clock = create_font(&state.cfg, state.dpi, CLOCK_EXTRA, true);

    // The controls' face is the theme too: a background edit has to move it or
    // the checkboxes keep the old plate until the next launch.
    let bg = background(&state.cfg);
    // SAFETY: the brush is ours; the window holds it in `UiState` and replaces
    // it here, so the old one is no longer referenced.
    unsafe {
        let _ = DeleteObject(HGDIOBJ(state.face_brush.0));
        state.face_brush = CreateSolidBrush(bg);
    }
}
