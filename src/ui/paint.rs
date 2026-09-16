//! The frame painter: the window's face, the sidebar, and the selected page.
//!
//! The page itself is laid out by walking a `Canvas` — see `components` — so
//! this file is a list of contents rather than a column of arithmetic, and it
//! never names an absolute row position. The two exceptions are deliberate and
//! commented where they are: the sparkline fills the *remainder* of the window,
//! and the Settings captions have to sit on the bands that `layout_settings`
//! put the controls on.

use crate::ui::components::{Canvas, Fonts};
use crate::ui::design::{S3, palette};
use crate::ui::pages::{
    DATA, PAGES, PORTS, SETTINGS, SPEEDTEST, SYSTEM, page_rows, page_section, page_shows_graph,
    usage_rows,
};
use crate::ui::settings::{SET_ROW_LABELS, foot_button_top};
use crate::ui::theme::scale;
use crate::ui::{PAD, ROW_H, SPARK_GAP, TITLE_EXTRA, TITLE_PAD, VALUE_OFFSET, UiState};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DEFAULT_GUI_FONT, DeleteObject, FillRect, GetDC, GetStockObject, HGDIOBJ,
    NULL_BRUSH, ReleaseDC, SelectObject, SetBkMode, TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

/// Draw one frame: background, sidebar, heading, the page's rows, chart, and
/// the confirmation popup if one is up.
///
/// `&mut` for one reason: the Ports page's rows are laid out here and hit-
/// tested in the window procedure, so their bands are recorded as they are
/// drawn. The alternative — the click path re-deriving the same sums — is two
/// answers to one question, which is the bug this avoids by construction.
pub(crate) fn paint(hwnd: HWND, state: &mut UiState) {
    // SAFETY: every GDI object created here is deleted before returning or
    // selected back out into the DC it came from, and the DC is released.
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

        let pal = palette(&state.cfg);
        let pad = scale(PAD, state.dpi);
        let side = crate::ui::theme::sidebar_w(state.dpi);
        let x0 = side + pad;
        let x1 = w - pad;
        if x1 <= x0 {
            return;
        }

        // One DC for the whole frame, so the background cannot land a frame
        // behind the text drawn on it.
        let dc = GetDC(Some(hwnd));
        if dc.is_invalid() {
            return;
        }
        let brush = CreateSolidBrush(pal.surface);
        FillRect(dc, &rect, brush);
        let _ = DeleteObject(HGDIOBJ(brush.0));
        SetBkMode(dc, TRANSPARENT);

        crate::ui::sidebar::paint(dc, &rect, state);

        let page = state.page.min(PAGES.len() - 1);
        let row_h = scale(ROW_H, state.dpi);
        let fonts = Fonts {
            body: state.font,
            bold: state.bold,
            title: state.title,
            // The caption shares the body face: a fourth face would need a fourth
            // `HFONT` in `UiState` and a fourth branch in `rebuild_fonts`, which
            // is not worth one header above a day list.
            // ponytail: caption == body; add a face when a page needs two.
            caption: state.font,
        };
        // The heading gets its own band — a title is larger than a row and has
        // air under it — and everything after it is rows on `row_h` bands,
        // nudge down from the band's top so a label and its value look level.
        let mut c = Canvas::new(
            dc,
            &fonts,
            &pal,
            x0,
            x1,
            row_h,
            scale(VALUE_OFFSET, state.dpi),
            pad,
        );
        // A title is larger than a row, and the first band is laid out from its
        // *top* — so the title would sit high in its band with a gap under it.
        // The gap above it is padded instead, which is what makes the block of
        // title and rows look centred in the window rather than pushed to the
        // ceiling. `layout_settings` moves its controls by the same amount.
        c.space(scale(TITLE_PAD, state.dpi));
        c.heading(PAGES[page], scale(ROW_H + TITLE_EXTRA * 2, state.dpi));

        // An over-quota window is red top to bottom, not red in a footnote: the
        // colour is a property of the page, so it is set once here.
        if state.model.quota_alert {
            c.emphasise(pal.danger);
        }

        // Which heading the page has drawn last, so a group opened by two rows
        // — "Connection" is either the SSID or the adapter, whichever is there —
        // draws one heading rather than one per row.
        let mut drawn: Option<&'static str> = None;
        for (label, value) in &page_rows(page, &state.model) {
            // A metric page is a wall of pairs, and ten of them with nothing
            // between them is a wall ten rows tall. The headings come from the
            // page's own anchor table and are drawn *on* the row they name, so
            // grouping costs no vertical space and cannot move a row out of
            // order. Anchored on the row, so a group whose first row is
            // switched off has no heading rather than one standing over someone
            // else's rows.
            if let Some(name) = page_section(page, label, drawn) {
                c.heading_row(name);
                drawn = Some(name);
            }
            c.row(label, value);
        }

        // The System page's volumes. They come through here rather than through
        // `page_rows` for the same reason the day list below does: a drive
        // letter is a runtime label, not a `&'static str` caption. Under a
        // caption of its own so a machine with three volumes reads as one group
        // instead of three stray rows under the power reading.
        if page == SYSTEM && !state.model.disks.is_empty() {
            c.section("Storage");
            for (mount, usage) in &state.model.disks {
                c.row(mount, usage);
            }
        }

        // The Data page's day-by-day breakdown, under the two totals above it.
        // Its rows carry runtime dates rather than the metric pages' fixed
        // captions, which is why they are not in `page_rows`.
        if page == DATA {
            let days = usage_rows(&state.model);
            if days.is_empty() {
                c.empty("No usage recorded yet.");
            } else {
                c.section("Recent days");
                for (day, bytes) in days {
                    c.row(&day, &crate::telemetry::usage::format_size(bytes));
                }
            }
        }

        // The Ports page's open-port list, under the four counters that summarise
        // it. Its rows carry runtime port numbers rather than captions, so they
        // come through here like the day list above. `section` claims a band of
        // its own, which is safe on this page: the last `heading_row` above is
        // on the counter row *before* this caption, and only a heading drawn on
        // the same band could collide with it.
        //
        // The rows are also the page's selection: clicking one aims the Stop
        // button at the process holding it, so their bands are recorded here
        // rather than recomputed by the click path.
        state.port_rows.clear();
        if page == PORTS {
            // The Stop button is pinned to the foot, so the list has to stop
            // above it — twelve ports plus four counters is taller than a short
            // window, and a row drawn under the button is a row that cannot be
            // clicked.
            let limit = foot_button_top(h, state.dpi) - scale(S3, state.dpi);
            if state.model.open_ports.is_empty() {
                c.empty("No listening ports were found.");
            } else if c.y() + row_h * 2 <= limit {
                c.section("Open ports");
                // Borrowed from the model and pushed to the state's own list in
                // the same loop: disjoint fields of one struct, so the borrow
                // checker has no argument with it.
                for entry in &state.model.open_ports {
                    if c.y() + row_h > limit {
                        break;
                    }
                    let top = c.y();
                    state.port_rows.push(RECT {
                        left: x0,
                        top,
                        right: x1,
                        bottom: top + row_h,
                    });
                    if state.selected_port == Some(entry.pid) {
                        // Behind the text, not around it: a ring drawn after the
                        // row would clip the descenders of its own label.
                        c.pill();
                    }
                    c.row(&entry.port, &entry.owner);
                }
            }
            // What the last stop did, under the list. A failed one is the
            // expected case rather than the strange one — an unelevated process
            // cannot end a service — so it is a sentence, not an alarm.
            if let Some(notice) = &state.port_notice {
                let colour = if notice.starts_with("stopped") {
                    pal.muted
                } else {
                    pal.danger
                };
                c.note(notice, colour);
            }
        }

        // The Speed Test page, above the Run button pinned to the foot of the
        // column. Everything here is conditional on a run having happened, so
        // the rows would otherwise rearrange themselves under the reader's eyes
        // — the price of the button being placed rather than laid out in this
        // list. Nothing may be drawn past the button's own top, which is why
        // the history list is trimmed to the room left rather than allowed to
        // run under it.
        if page == SPEEDTEST {
            let limit = foot_button_top(h, state.dpi) - scale(S3, state.dpi);
            if state.model.speed_running {
                let pct = state.model.speed_percent.min(100);
                c.progress("Progress", &format!("{pct}%"), pct);
            }
            // A failure is a row, not a footnote: it is the result of the run,
            // and it is the thing the reader opened the page to find out.
            if !state.model.speed_error_text.is_empty() {
                c.note(&state.model.speed_error_text, pal.danger);
            }
            // The caption claims a band of its own before its first row, so the
            // check has to cover both or a caption would be left standing with
            // no list under it.
            if !state.model.speed_history.is_empty() && c.y() + row_h * 2 <= limit {
                c.section("History");
                for (when, result) in &state.model.speed_history {
                    if c.y() + row_h > limit {
                        break;
                    }
                    c.row(when, result);
                }
            }
        }

        // The Settings page's captions. They sit on the same bands as the
        // controls `layout_settings` places, from the same constants, so a row
        // and its caption cannot drift even though two functions draw them.
        if page == SETTINGS {
            for label in SET_ROW_LABELS {
                if label.is_empty() {
                    // The tile grid's rows. A checkbox carries its own label, so
                    // the band is skipped rather than closed up: the controls are
                    // placed by row index, and the captions have to keep step.
                    c.space(row_h);
                } else {
                    c.row(label, "");
                }
            }
            // The notice sits under the buttons rather than beside them: it is
            // the result of the whole page, not of either button, and it needs
            // the full width to name a path that did not write.
            if let Some(notice) = &state.notice {
                let colour = if notice.starts_with("saved") {
                    pal.text
                } else {
                    pal.danger
                };
                c.note(notice, colour);
            }
        }

        // The chart fills whatever room is left, so the picture grows with the
        // window instead of sitting in a fixed corner. It decides for itself
        // whether the rectangle is big enough once its gutter and its time strip
        // are taken out of it, which is a question only it can answer.
        let spark_gap = scale(SPARK_GAP, state.dpi);
        let spark_top = c.y() + spark_gap;
        let spark_h = h - spark_gap - pad - spark_top;
        if page_shows_graph(page) && spark_h > 0 && x1 > x0 {
            crate::ui::chart::paint(
                &c,
                RECT {
                    left: x0,
                    top: spark_top,
                    right: x1,
                    bottom: spark_top + spark_h,
                },
                &state.model.history,
                state.cfg.interval_ms,
                state.dpi,
            );
        }

        // The confirmation popup, last and over everything. It dims the whole
        // client area rather than the content column, because the sidebar is
        // part of what the popup is standing in front of — and a modal that
        // leaves its own window's navigation clickable is not modal.
        crate::ui::modal::paint(dc, &rect, &state.modal, &fonts, &pal, state.dpi);

        // Leave the DC holding stock objects rather than ours.
        SelectObject(dc, GetStockObject(NULL_BRUSH));
        SelectObject(dc, GetStockObject(DEFAULT_GUI_FONT));
        ReleaseDC(Some(hwnd), dc);
    }
}
