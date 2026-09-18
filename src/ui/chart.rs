//! The history chart: an area plot with both axes labelled.
//!
//! Lifted out of the frame painter when the axes arrived. A bare polyline in
//! whatever room was left over is a dozen lines of arithmetic; a chart with a
//! gutter for its value labels, a strip for its time labels, a grid to read the
//! line against and a fill under it is enough of it to own a module — and
//! enough that the alternative was five more locals threaded through the one
//! function that also lays out the whole page.
//!
//! It is a *painter*, not a page: it takes the rectangle it was given and draws
//! in it, and the caller decides where the rectangle is. That is the same
//! division the rest of the window makes — the painter fills the remainder with
//! the picture, and the picture never asks how much remainder there is.
//!
//! Readings, not prose: the axis values are byte rates from `format_rate`, so a
//! chart's left edge and the `Download` row above it are the same units.

use crate::taskbar::format_rate;
use crate::taskbar::render::sparkline_points;
use crate::ui::components::{draw, hairline, Canvas};
use crate::ui::design::{mix, Palette, S1, S2};
use crate::ui::theme::scale;
use windows::Win32::Foundation::{COLORREF, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    CreatePen, CreateSolidBrush, DeleteObject, GetStockObject, Polygon, Polyline, SelectObject,
    DT_LEFT, DT_RIGHT, HGDIOBJ, NULL_PEN, PS_SOLID,
};

/// The column the value labels are right-aligned in. Wide enough for the
/// longest rate `format_rate` produces — `999.9M/s` — because a label ellipsised
/// at the axis is worse than no label: it reads as a value someone truncated.
const Y_GUTTER: i32 = 56;

/// The strip under the plot the time labels sit in.
const X_STRIP: i32 = 18;

/// How many horizontal rules the grid draws, the axis included. Three is the
/// fewest that reads as a scale rather than as a stray line.
const LEVELS: i32 = 3;

/// How far the fill under the line is mixed toward the page.
///
/// Low on purpose: the fill is the line's weight, not a second colour. A denser
/// tint would compete with the line drawn on top of it and with the values in
/// the gutter beside it.
const FILL_PCT: u32 = 16;

/// The time span `samples` of `interval_ms` cover, oldest to newest, as an axis
/// label: `0s`, `45s`, `2m`, `1h`.
///
/// The gap between two samples, not the number of them: sixty readings a second
/// apart span fifty-nine seconds, and a label reading `1m ago` over a chart that
/// covers 59 of them would be wrong by exactly the amount this exists to state.
pub(crate) fn span_label(samples: usize, interval_ms: u32) -> String {
    let secs = (samples.saturating_sub(1) as u64) * u64::from(interval_ms) / 1000;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
}

/// The left edge's label: how far back the chart reaches.
pub(crate) fn ago_label(samples: usize, interval_ms: u32) -> String {
    format!("{} ago", span_label(samples, interval_ms))
}

/// Draw the chart in `area`, or draw nothing and answer `false` when there is
/// no room or no history.
///
/// The caller has already decided the area is worth offering — but it cannot
/// know that the plot inside it survives the gutter and the label strip, which
/// is the one thing this can still refuse.
pub(crate) fn paint(c: &Canvas, area: RECT, history: &[u64], interval_ms: u32, dpi: u32) -> bool {
    if history.len() < 2 {
        return false;
    }
    let plot = RECT {
        left: area.left + scale(Y_GUTTER, dpi),
        top: area.top,
        right: area.right,
        bottom: area.bottom - scale(X_STRIP, dpi),
    };
    let plot_w = plot.right - plot.left;
    let plot_h = plot.bottom - plot.top;
    // A plot a few pixels across is a mark, not a chart: at that size the grid
    // and the labels would be most of what is drawn and the line would read as
    // a stray diagonal.
    if plot_w < scale(24, dpi) || plot_h < scale(16, dpi) {
        return false;
    }

    // The line is scaled to the window's own maximum rather than to a fixed
    // ceiling. A monitor showing 20M/s and one showing 900M/s both fill the
    // plot, and the absolute value is what the gutter labels are for.
    let max = history.iter().copied().max().unwrap_or(0).max(1);

    let points: Vec<POINT> = sparkline_points(history, plot_w, plot_h)
        .into_iter()
        .map(|p| POINT {
            x: p.x + plot.left,
            y: p.y + plot.top,
        })
        .collect();
    if points.len() < 2 {
        return false;
    }

    grid(c, &plot, max, dpi);
    fill(c.dc, &plot, &points, c.pal);
    line(c.dc, &points, c.pal.accent);
    times(c, &plot, history.len(), interval_ms, dpi);
    true
}

/// The horizontal rules, and the value each one stands for in the gutter.
///
/// Top-down: level `LEVELS - 1` is the window's maximum and level 0 is the
/// baseline, which is the axis itself and is drawn in full rather than as a
/// faint rule — it is the one line the reader is measuring from.
fn grid(c: &Canvas, plot: &RECT, max: u64, dpi: u32) {
    let plot_h = plot.bottom - plot.top;
    for level in (0..LEVELS).rev() {
        let y = plot.top + plot_h * (LEVELS - 1 - level) / (LEVELS - 1);
        let colour = if level == 0 {
            c.pal.border
        } else {
            mix(c.pal.surface, c.pal.border, 55)
        };
        // SAFETY: `c.dc` is a live DC for the frame; `hairline` documents its
        // own contract.
        unsafe {
            hairline(c.dc, plot.left, plot.right, y, colour);
        }
        let value = max * level as u64 / (LEVELS - 1) as u64;
        // Centred on its rule: a label sitting with its top at the line reads as
        // belonging to the band above it.
        let top = y - c.row_h / 2 + c.nudge;
        // SAFETY: a live DC and the frame's own font.
        unsafe {
            draw(
                c.dc,
                c.fonts.caption,
                &format_rate(value),
                plot.left - scale(Y_GUTTER, dpi),
                top,
                plot.left - scale(S2, dpi),
                DT_RIGHT,
            );
        }
    }
}

/// The area under the line, closed along the baseline.
fn fill(dc: windows::Win32::Graphics::Gdi::HDC, plot: &RECT, points: &[POINT], pal: &Palette) {
    let mut shape: Vec<POINT> = points.to_vec();
    shape.push(POINT {
        x: plot.right,
        y: plot.bottom,
    });
    shape.push(POINT {
        x: plot.left,
        y: plot.bottom,
    });
    // SAFETY: the brush and pen are created here, selected, replaced and
    // destroyed inside the same block, so the DC never outlives either.
    unsafe {
        let brush = CreateSolidBrush(mix(pal.surface, pal.accent, FILL_PCT));
        let old_brush = SelectObject(dc, HGDIOBJ(brush.0));
        // `Polygon` strokes its outline as it fills, so it gets the null pen —
        // otherwise the shape wears a border in whatever pen was last selected.
        let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
        let _ = Polygon(dc, &shape);
        SelectObject(dc, old_pen);
        SelectObject(dc, old_brush);
        let _ = DeleteObject(HGDIOBJ(brush.0));
    }
}

/// The line itself, one pixel of accent over the fill.
fn line(dc: windows::Win32::Graphics::Gdi::HDC, points: &[POINT], colour: COLORREF) {
    // SAFETY: the pen is created here, selected, then replaced and destroyed.
    unsafe {
        let pen = CreatePen(PS_SOLID, 1, colour);
        let old = SelectObject(dc, HGDIOBJ(pen.0));
        let _ = Polyline(dc, points);
        SelectObject(dc, old);
        let _ = DeleteObject(HGDIOBJ(pen.0));
    }
}

/// The time labels under the plot: how far back the left edge reaches, and when
/// the right edge is.
///
/// The right edge is `now` rather than a clock time: the newest sample is the
/// current reading, so naming the hour would be a fact about the machine's
/// clock sitting where a fact about the chart belongs.
fn times(c: &Canvas, plot: &RECT, samples: usize, interval_ms: u32, dpi: u32) {
    let top = plot.bottom + scale(S1, dpi);
    // SAFETY: a live DC and the frame's own font.
    unsafe {
        draw(
            c.dc,
            c.fonts.caption,
            &ago_label(samples, interval_ms),
            plot.left,
            top,
            plot.left + (plot.right - plot.left) / 2,
            DT_LEFT,
        );
        draw(
            c.dc,
            c.fonts.caption,
            "now",
            plot.left + (plot.right - plot.left) / 2,
            top,
            plot.right,
            DT_RIGHT,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The span is the *gap* between samples, not the count of them. Off by one
    /// here is a chart whose axis disagrees with the history it plots, which is
    /// the kind of thing nobody notices until they use it to size a download.
    #[test]
    fn the_axis_names_the_gap_between_samples() {
        // Sixty readings a second apart: fifty-nine seconds of history.
        assert_eq!(span_label(60, 1000), "59s");
        assert_eq!(ago_label(60, 1000), "59s ago");

        // One sample covers no time at all, and must not underflow into a
        // hundred and eighty-four quintillion seconds.
        assert_eq!(span_label(1, 1000), "0s");
        assert_eq!(span_label(0, 1000), "0s");
    }

    /// The unit changes where the reader stops counting seconds, and the label
    /// stays short enough for the strip at every interval the config allows.
    #[test]
    fn the_units_roll_over_and_never_run_long() {
        assert_eq!(span_label(2, 500), "0s");
        assert_eq!(span_label(5, 10_000), "40s");
        assert_eq!(span_label(13, 10_000), "2m");
        assert_eq!(span_label(361, 10_000), "1h");

        // The widest interval over a full window is an hour at most, which is
        // what the strip under the plot was sized for.
        for samples in [1usize, 60, 3600] {
            assert!(span_label(samples, 10_000).len() <= 4, "label too wide");
        }
    }
}
