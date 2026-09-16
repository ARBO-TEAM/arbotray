//! 96-DPI metrics, the sidebar's placement and the font rebuild.

use crate::ui::fonts::create_font;
use crate::ui::settings::layout_settings;
use crate::ui::theme::{background, shade, sidebar_w};
use crate::ui::UiState;
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateSolidBrush, DeleteObject, HGDIOBJ};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, SWP_NOACTIVATE, SWP_NOZORDER, SendMessageW, SetWindowPos, WM_SETFONT,
};

/// Metrics in 96-DPI pixels. The font and the sidebar width carry the scaling;
/// these are the ratios everything else is laid out from.
pub(crate) const PAD: i32 = 20;
pub(crate) const ROW_H: i32 = 30;
pub(crate) const VALUE_OFFSET: i32 = 6;
pub(crate) const SPARK_GAP: i32 = 14;
pub(crate) const SIDEBAR_W: i32 = 150;
pub(crate) const TITLE_EXTRA: i32 = 6;

/// Smallest size still showing every row without the frame collapsing. Wider
/// than the single-column window was, because the sidebar eats its share.
pub(crate) const MIN_W: i32 = 520;
pub(crate) const MIN_H: i32 = 320;

/// Initial size: room for the rows plus a decent sparkline.
pub(crate) const START_W: i32 = 720;
pub(crate) const START_H: i32 = 460;

/// What the window knows between repaints. Boxed and hung off the window's
/// Put the sidebar where it belongs and give it the current font. Called on
/// every resize, which is also what keeps it correct across a DPI change.
pub(crate) fn layout(hwnd: HWND, state: &mut UiState) {
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

/// Delete and rebuild the three faces from the current config and DPI. Shared
/// by the DPI change and the settings save, because both are "the font's inputs
/// moved" and getting one of the two paths wrong leaves stale text.
pub(crate) fn rebuild_fonts(state: &mut UiState) {
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
