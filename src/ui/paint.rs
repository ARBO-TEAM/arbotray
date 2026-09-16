//! The content painter and its single-line text helper.

use crate::taskbar::render::{parse_color, sparkline_points};
use crate::ui::pages::{DATA, PAGES, SETTINGS, page_rows, page_shows_graph, usage_rows};
use crate::ui::settings::{SET_ROW_COUNT, SET_ROW_LABELS};
use crate::ui::theme::{background, fg_default, scale, shade, sidebar_w};
use crate::ui::{PAD, ROW_H, SPARK_GAP, TITLE_EXTRA, VALUE_OFFSET, UiState};
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    CreatePen, CreateSolidBrush, DEFAULT_GUI_FONT, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT,
    DT_SINGLELINE, DeleteObject, DrawTextW, FillRect, GetDC, GetStockObject, HGDIOBJ, HFONT,
    NULL_BRUSH, PS_SOLID, Polyline, ReleaseDC, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

/// Draw the content area: background, heading, rows, sparkline.
pub(crate) fn paint(hwnd: HWND, state: &UiState) {
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
pub(crate) unsafe fn draw(
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
