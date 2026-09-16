//! The reusable drawing primitives, and the text helper everything is built on.
//!
//! A page is assembled by walking a `Canvas`: each call draws one component and
//! advances the vertical cursor. The painter then reads as the page's own
//! contents — heading, section, five rows, a note — with no arithmetic between
//! the lines of the list, which is where a hand-laid-out window usually goes
//! wrong. Adding a row to a page is one call, and it cannot push the rows below
//! it out of line because it never sees their coordinates.
//!
//! Nothing here knows about a page, the model or the config. It draws a rounded
//! rectangle, a hairline, a glyph and a label-value pair, which is everything
//! the four metric pages and the settings page are actually made of.

use crate::ui::design::{GROUP_COL, ICON_COL, Palette, S1, S2, S3, S6};
use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    CreatePen, CreateSolidBrush, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE,
    DeleteObject, DrawTextW, GetStockObject, HDC, HGDIOBJ, HFONT, NULL_PEN, PS_SOLID,
    Polyline, RoundRect, SelectObject, SetTextColor,
};

/// The row height is generous on purpose: a single-line `DrawTextW` clips at the
/// rectangle's bottom and can never reposition from it, so the bottom only has
/// to be far enough away that a heading taller than a row still fits through
/// this one function.
const RUN_H: i32 = 400;

/// One single-line run of text, starting at `top`.
///
/// Single-line text is laid out from the top of the rectangle, so `top` is the
/// text's own top: callers add a nudge for a row's band.
///
/// # Safety
/// `dc` must be a live DC and `font` a live font.
pub(crate) unsafe fn draw(
    dc: HDC,
    font: HFONT,
    text: &str,
    left: i32,
    top: i32,
    right: i32,
    align: windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT,
) {
    if text.is_empty() {
        return;
    }
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    let mut rect = RECT {
        left,
        top,
        right,
        bottom: top + RUN_H,
    };
    // SAFETY: the font and DC are the caller's, both live; `wide` and `rect`
    // outlive the call.
    unsafe {
        SelectObject(dc, HGDIOBJ(font.0));
        DrawTextW(dc, &mut wide, &mut rect, DT_SINGLELINE | DT_NOPREFIX | align);
    }
}

/// One codepoint from the icon face, drawn at `x`.
///
/// # Safety
/// `dc` must be a live DC and `font` the icon face.
pub(crate) unsafe fn glyph(dc: HDC, font: HFONT, cp: u16, x: i32, top: i32) {
    let text = String::from_utf16_lossy(&[cp]);
    // SAFETY: delegated straight to `draw`, whose contract this repeats.
    unsafe {
        draw(dc, font, &text, x, top, x + ICON_COL, DT_LEFT);
    }
}

/// Fill a rounded rectangle with no outline.
///
/// `RoundRect` strokes as it fills, so it is handed the stock null pen: without
/// it every card and every selection pill would wear a one-pixel border in the
/// current pen's colour, which is the *previous* component's colour.
///
/// # Safety
/// `dc` must be a live DC.
pub(crate) unsafe fn rounded_fill(dc: HDC, rect: RECT, radius: i32, colour: COLORREF) {
    // SAFETY: the brush and pen are created here, selected, and destroyed after
    // being replaced — so the DC never outlives either. The creation is inside
    // the block because edition 2024 wants every unsafe call spelled out, even
    // in an already-unsafe body.
    unsafe {
        let brush = CreateSolidBrush(colour);
        let old_brush = SelectObject(dc, HGDIOBJ(brush.0));
        let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
        let _ = RoundRect(
            dc,
            rect.left,
            rect.top,
            rect.right,
            rect.bottom,
            radius,
            radius,
        );
        SelectObject(dc, old_pen);
        SelectObject(dc, old_brush);
        let _ = DeleteObject(HGDIOBJ(brush.0));
    }
}

/// A one-pixel horizontal rule from `x0` to `x1` at `y`.
///
/// # Safety
/// `dc` must be a live DC.
pub(crate) unsafe fn hairline(dc: HDC, x0: i32, x1: i32, y: i32, colour: COLORREF) {
    // SAFETY: the pen is created here, selected, then replaced and destroyed.
    unsafe {
        let pen = CreatePen(PS_SOLID, 1, colour);
        let old = SelectObject(dc, HGDIOBJ(pen.0));
        let line = [
            windows::Win32::Foundation::POINT { x: x0, y },
            windows::Win32::Foundation::POINT { x: x1, y },
        ];
        let _ = Polyline(dc, &line);
        SelectObject(dc, old);
        let _ = DeleteObject(HGDIOBJ(pen.0));
    }
}

/// The faces a component draws with, gathered once per frame.
///
/// The icon face is not among them: only the sidebar draws glyphs, and it sizes
/// its own from `design::icon_font`, so carrying a fifth handle here would be a
/// field every page pays for and one of them uses.
pub(crate) struct Fonts {
    pub body: HFONT,
    /// One step heavier, for values, so the numbers lead and the captions
    /// annotate.
    pub bold: HFONT,
    /// The page heading.
    pub title: HFONT,
    /// A section caption, between the body and the heading.
    pub caption: HFONT,
}

/// A vertical cursor down one page.
///
/// Every method draws at the cursor and moves it past what it drew, so the
/// caller never adds two numbers together to find out where something goes.
///
/// The canvas also owns the DC's text colour: each method selects the colour
/// its own kind of text is drawn in, so a page reads as a list of contents
/// rather than as a list of `SetTextColor` calls with the contents between
/// them.
pub(crate) struct Canvas<'a> {
    dc: HDC,
    fonts: &'a Fonts,
    pal: &'a Palette,
    /// Content column.
    x0: i32,
    x1: i32,
    /// Where the next component starts.
    y: i32,
    row_h: i32,
    /// A row's text sits a little below its band's top, which is what makes a
    /// label and its value look level.
    nudge: i32,
    /// Replaces the palette's body colour for rows. Set for a whole page — an
    /// over-quota window is red top to bottom rather than red in a footnote.
    emph: Option<COLORREF>,
    /// Whether rows are drawn past the group-heading column.
    ///
    /// Sticky rather than per-row: a heading is drawn on its group's *first*
    /// row's band, and if only that row moved, the heading would sit beside it
    /// and every row under it would start in a different place — a group that
    /// looks ragged. Once a page has a heading, all of its rows share the
    /// column the heading left for them.
    indented: bool,
}

impl<'a> Canvas<'a> {
    pub(crate) fn new(
        dc: HDC,
        fonts: &'a Fonts,
        pal: &'a Palette,
        x0: i32,
        x1: i32,
        row_h: i32,
        nudge: i32,
        y: i32,
    ) -> Self {
        Self {
            dc,
            fonts,
            pal,
            x0,
            x1,
            y,
            row_h,
            nudge,
            emph: None,
            indented: false,
        }
    }

    /// Draw the rest of this page's rows in `colour`.
    pub(crate) fn emphasise(&mut self, colour: COLORREF) {
        self.emph = Some(colour);
    }

    /// The colour body text is drawn in.
    fn text_colour(&self) -> COLORREF {
        self.emph.unwrap_or(self.pal.text)
    }

    /// The left edge a row's label is drawn from. Past the heading column once
    /// this page has drawn one, so a label never collides with the heading on
    /// its own band.
    fn label_x(&self) -> i32 {
        if self.indented {
            self.x0 + GROUP_COL + S2
        } else {
            self.x0
        }
    }

    /// Where the cursor is. The painter uses it to place the sparkline under
    /// everything else.
    pub(crate) fn y(&self) -> i32 {
        self.y
    }

    /// Skip `height` pixels. The one allowance the layering makes for a
    /// component that is not a row.
    pub(crate) fn space(&mut self, height: i32) {
        self.y += height;
    }

    /// The page's own title, in a band `band` tall.
    ///
    /// The band is the caller's because a heading is not a row: it is a couple
    /// of points larger with air beneath it, and whatever follows starts below
    /// where it actually ends.
    pub(crate) fn heading(&mut self, text: &str, band: i32) {
        let top = self.y;
        // SAFETY: a live DC and fonts owned by the caller's frame.
        unsafe {
            SetTextColor(self.dc, self.text_colour());
            draw(
                self.dc,
                self.fonts.title,
                text,
                self.x0,
                top,
                self.x1,
                DT_LEFT,
            );
        }
        self.y += band;
    }

    /// A muted caption naming the group of rows that follow, with a rule under
    /// it, on a band of its own.
    ///
    /// This is the *standalone* group caption, for a list whose rows are not
    /// page rows — the Data page's day list, whose labels are runtime dates. A
    /// metric page uses `heading_row` instead, which costs no vertical space.
    pub(crate) fn section(&mut self, text: &str) {
        self.space(S3);
        // SAFETY: as above; `hairline` documents its own contract.
        unsafe {
            SetTextColor(self.dc, self.pal.muted);
            draw(
                self.dc,
                self.fonts.caption,
                &text.to_uppercase(),
                self.x0,
                self.y,
                self.x1,
                DT_LEFT,
            );
            let rule = self.y + self.row_h - S1;
            hairline(self.dc, self.x0, self.x1, rule, self.pal.border);
        }
        self.y += self.row_h;
    }

    /// A group heading drawn *on* the band of the row it names, with a faint
    /// rule filling the space beside it.
    ///
    /// It costs no vertical space and moves nothing, which is the whole reason
    /// the group headings can be added to pages that are already laid out: a
    /// heading that claimed a band of its own would push every row below it
    /// down, and the row order — which is the thing the pages are actually
    /// careful about — would then depend on where someone put a caption.
    ///
    /// The heading is drawn at the content edge and the rows it governs are
    /// indented past the column it leaves (`label_x`), so the two never share a
    /// pixel. It is *not* ellipsised: it is clipped to its column, because a
    /// heading cut off mid-word still reads as a heading, while one ending in an
    /// ellipsis reads as a value someone truncated.
    pub(crate) fn heading_row(&mut self, text: &str) {
        self.indented = true;
        // SAFETY: as above; `hairline` documents its own contract.
        unsafe {
            SetTextColor(self.dc, self.pal.muted);
            let right = self.x0 + GROUP_COL;
            draw(
                self.dc,
                self.fonts.caption,
                &text.to_uppercase(),
                self.x0 + S1,
                self.y + self.nudge,
                right,
                DT_LEFT,
            );
            hairline(
                self.dc,
                right + S2,
                self.x1,
                self.y + self.row_h / 2,
                self.pal.border,
            );
        }
    }

    /// A caption with its value right-aligned on the same band.
    ///
    /// The label's left edge is `label_x`, which is where the group headings
    /// pushed it once this page drew one; the value stays pinned to the right
    /// edge either way, so the values column of a page is a single line down it
    /// regardless of how the labels are indented.
    pub(crate) fn row(&mut self, label: &str, value: &str) {
        // SAFETY: a live DC and fonts owned by the caller's frame.
        unsafe {
            SetTextColor(self.dc, self.text_colour());
            draw(
                self.dc,
                self.fonts.body,
                label,
                self.label_x(),
                self.y + self.nudge,
                self.x1,
                DT_LEFT,
            );
            // Ellipsised rather than clipped: an adapter description and a list
            // of resolvers can both outrun the value column, and half a word
            // looks like a rendering fault while `Realtek PCIe GbE F…` does not.
            draw(
                self.dc,
                self.fonts.bold,
                value,
                self.x0,
                self.y + self.nudge,
                self.x1,
                DT_RIGHT | DT_END_ELLIPSIS,
            );
        }
        self.y += self.row_h;
    }

    /// A status line under the content rather than in it: what the page did,
    /// not what it is showing.
    pub(crate) fn note(&mut self, text: &str, colour: COLORREF) {
        // SAFETY: a live DC and the caller's font.
        unsafe {
            SetTextColor(self.dc, colour);
            draw(
                self.dc,
                self.fonts.body,
                text,
                self.x0,
                self.y + self.nudge,
                self.x1,
                DT_LEFT,
            );
        }
        self.y += self.row_h;
    }

    /// An empty-state line for a page with nothing to report yet.
    pub(crate) fn empty(&mut self, text: &str) {
        self.space(S6);
        // SAFETY: a live DC and the caller's font.
        unsafe {
            SetTextColor(self.dc, self.pal.muted);
            draw(
                self.dc,
                self.fonts.body,
                text,
                self.x0,
                self.y,
                self.x1,
                DT_LEFT,
            );
        }
        self.y += self.row_h;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cursor is the module's whole contract: a page is a list of calls and
    /// every one of them has to move it, or two components land on one band.
    /// Driven through a null DC, so this touches only the arithmetic.
    #[test]
    fn every_component_moves_the_cursor_past_itself() {
        let fonts = Fonts {
            body: HFONT::default(),
            bold: HFONT::default(),
            title: HFONT::default(),
            caption: HFONT::default(),
        };
        let pal = crate::ui::design::palette(&crate::config::Config::default());
        // A null DC: the text calls no-op against it and the arithmetic under
        // test never touches it.
        let mut c = Canvas::new(HDC::default(), &fonts, &pal, 0, 100, 30, 6, 0);

        let start = c.y();
        c.heading("Overview", 44);
        assert_eq!(c.y() - start, 44, "a heading owns the band it was given");

        let after_heading = c.y();
        c.section("Traffic");
        assert!(
            c.y() - after_heading >= 30,
            "a section owns a band plus its own gap"
        );

        let before_row = c.y();
        c.row("Download", "1.4G");
        assert_eq!(c.y() - before_row, 30, "a row is exactly one band");

        // The load-bearing property of the page-wide grouping: a heading rides
        // on a row's band, so adding one to a page cannot move any row. If this
        // ever moves, every grouping assertion over row *order* still passes
        // while the pages have quietly grown taller than the window.
        let before_head = c.y();
        c.heading_row("Traffic");
        assert_eq!(c.y(), before_head, "a group heading must not claim a band");

        let before_note = c.y();
        c.note("saved", pal.text);
        assert_eq!(c.y() - before_note, 30);

        // The one component that must *not* move it: the empty state stands in
        // for rows that are absent, so it takes its own gap and one band.
        let before_empty = c.y();
        c.empty("No usage recorded yet.");
        assert_eq!(c.y() - before_empty, 30 + 24);
    }

}
