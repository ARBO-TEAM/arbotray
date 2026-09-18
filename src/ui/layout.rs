//! 96-DPI metrics and the font rebuild.

use crate::ui::fonts::create_font;
use crate::ui::settings::layout_settings;
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

/// The floor is the sidebar's own height, not a number chosen beside it.
///
/// It was a literal 340 until a tenth page was added, at which point the list
/// needed 352 and the last entry fell under the frame — clickable nowhere,
/// visible nowhere, and nothing in the build able to notice. Derived now, so
/// the eleventh page moves the floor by itself.
pub(crate) const MIN_H: i32 = sidebar_min_h();

/// Height the page list needs: its top inset plus one band per page.
///
/// `const fn` so it is computed at compile time and can sit in a `const` — the
/// point is that no build can exist in which this disagrees with the sidebar.
const fn sidebar_min_h() -> i32 {
    let list = crate::ui::sidebar::TOP
        + crate::ui::design::ITEM_H * crate::ui::pages::PAGES.len() as i32;
    // A window shorter than its own content is unusable, but so is one with no
    // room for the content beside the list; 340 was the tested comfortable
    // floor for the pages themselves, so the taller of the two wins.
    if list > 340 { list } else { 340 }
}

/// Initial size: room for the rows plus a decent sparkline.
///
/// `START_H` tracks the System page's card stack — the tallest page now that it
/// is five cards rather than eleven rows. A card wears `2*CARD_PAD + CHIP + S2`
/// of chrome over its rows, so the stack needs ~860 where the rows needed 580.
pub(crate) const START_W: i32 = 720;
pub(crate) const START_H: i32 = 960;

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
        for font in [
            &mut state.font,
            &mut state.bold,
            &mut state.title,
            &mut state.clock,
        ] {
            let _ = DeleteObject(HGDIOBJ(font.0));
        }
    }
    state.font = create_font(&state.cfg, state.dpi, 0, false);
    state.bold = create_font(&state.cfg, state.dpi, 1, true);
    state.title = create_font(&state.cfg, state.dpi, TITLE_EXTRA, true);
    state.clock = create_font(&state.cfg, state.dpi, CLOCK_EXTRA, true);

    // The controls' face is the theme too: a background edit has to move it or
    // the checkboxes keep the old plate until the next launch. The plate is the
    // **card's** colour rather than the page's, because every settings control
    // stands on a card — handing them the page background draws a squared hole
    // in the plate under each one.
    let pal = crate::ui::design::palette(&state.cfg);
    // SAFETY: both brushes are ours; the window holds them in `UiState` and
    // replaces them here, so the old ones are no longer referenced.
    unsafe {
        let _ = DeleteObject(HGDIOBJ(state.face_brush.0));
        state.face_brush = CreateSolidBrush(pal.card);
        // The edit boxes' plate, on the same path and for the same reason: an
        // edit asks for its own background, and a save that moved the theme and
        // left this behind would leave nine fields in the old scheme's grey.
        let _ = DeleteObject(HGDIOBJ(state.field_brush.0));
        state.field_brush = CreateSolidBrush(pal.field);
    }
}
