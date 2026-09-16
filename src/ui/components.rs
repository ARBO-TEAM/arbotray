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

use crate::ui::design::{ICON_COL, Palette, S1, S3, S6};
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
    /// it. A metric page is a wall of label-value pairs, and the section is what
    /// turns the wall into two or three readable groups.
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

    /// A caption with its value right-aligned on the same band.
    pub(crate) fn row(&mut self, label: &str, value: &str) {
        // SAFETY: a live DC and fonts owned by the caller's frame.
        unsafe {
            SetTextColor(self.dc, self.text_colour());
            draw(
                self.dc,
                self.fonts.body,
                label,
                self.x0,
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
