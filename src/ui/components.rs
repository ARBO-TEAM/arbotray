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

use crate::ui::design::{
    CARD_PAD, CARD_RADIUS, CHIP, CHIP_TINT_PCT, GROUP_COL, ICON_CALENDAR, ICON_COL, LANE_GAP,
    Palette, RADIUS, S1, S2, S3, S6, TILE_H, TRACK_H,
};
use crate::ui::theme::scale;
use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    DT_CALCRECT, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE,
    DT_VCENTER, DT_WORDBREAK, CreatePen, CreateSolidBrush, DeleteObject, DrawTextW, GetStockObject,
    HDC, HGDIOBJ, HFONT, NULL_PEN, PS_SOLID, Polyline, RoundRect, SelectObject, SetTextColor,
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

/// One run of text centred inside `rect` rather than starting at a point.
///
/// The sibling `draw` cannot do this: it lays text out from the top of a
/// rectangle whose bottom it does not care about, which is right for a row's
/// band and wrong for a chip, where the glyph has to sit in the middle of a
/// shape whose height is the shape's own business. `DT_VCENTER` earns its keep
/// here and only here — it centres against the rectangle, so the caller never
/// measures a glyph's height, which is not a number GDI will hand over anyway.
///
/// # Safety
/// `dc` must be a live DC and `font` a live font.
pub(crate) unsafe fn draw_in(
    dc: HDC,
    font: HFONT,
    text: &str,
    rect: RECT,
    align: windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT,
) {
    if text.is_empty() {
        return;
    }
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    let mut rect = rect;
    // SAFETY: the font and DC are the caller's, both live; `wide` and `rect`
    // outlive the call.
    unsafe {
        SelectObject(dc, HGDIOBJ(font.0));
        DrawTextW(
            dc,
            &mut wide,
            &mut rect,
            DT_SINGLELINE | DT_NOPREFIX | DT_VCENTER | DT_CENTER | align,
        );
    }
}

/// One multi-line run of text, wrapped at word boundaries inside `rect`.
///
/// The sibling of `draw`, and the only caller is the confirmation popup's body:
/// a sentence that has to fit a card is the one string in the window whose
/// breaks are not already known to the caller.
///
/// # Safety
/// `dc` must be a live DC and `font` a live font.
pub(crate) unsafe fn draw_block(
    dc: HDC,
    font: HFONT,
    text: &str,
    rect: RECT,
    align: windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT,
) {
    if text.is_empty() {
        return;
    }
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    let mut rect = rect;
    // SAFETY: the font and DC are the caller's, both live; `wide` and `rect`
    // outlive the call.
    unsafe {
        SelectObject(dc, HGDIOBJ(font.0));
        DrawTextW(dc, &mut wide, &mut rect, DT_WORDBREAK | DT_NOPREFIX | align);
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

/// A glyph in a tinted plate: the mark a card or a tile leads with.
///
/// Plate and glyph are one call because they are one object. The tint exists
/// only to lift the glyph off whatever it is standing on, so a plate drawn by
/// one line and a glyph centred by another is two places to be wrong about the
/// same shape — and the failure is a glyph half out of its own chip, which
/// reads as a rendering fault rather than as a misplaced number.
///
/// `circle` picks the shape: a stat tile's mark is a circle and a card's is a
/// rounded square, and the two appear in the same window, so the shape is part
/// of what tells a group heading from a reading.
///
/// # Safety
/// `dc` must be a live DC and `font` the icon face.
pub(crate) unsafe fn chip(
    dc: HDC,
    font: HFONT,
    cp: u16,
    rect: RECT,
    tint: COLORREF,
    surface: COLORREF,
    circle: bool,
    dpi: u32,
) {
    let radius = if circle {
        // A circle is a rounded rectangle with the radius at its limit, which
        // keeps this to one `RoundRect` rather than a second ellipse path.
        (rect.bottom - rect.top) / 2
    } else {
        crate::ui::theme::scale(RADIUS, dpi)
    };
    // SAFETY: `rounded_fill` and `draw_in` each document their own contract.
    unsafe {
        rounded_fill(dc, rect, radius, crate::ui::design::mix(surface, tint, CHIP_TINT_PCT));
        SetTextColor(dc, tint);
        draw_in(dc, font, &String::from_utf16_lossy(&[cp]), rect, DT_LEFT);
    }
}

/// The width `text` needs in `font`, in pixels.
///
/// `DT_CALCRECT` writes the box the text would occupy into the rectangle it is
/// given, which is the only way GDI hands over a width — it has no measure call
/// of its own. A null DC (the tests) answers zero, which is fine: the one caller
/// is a pill's own width.
///
/// # Safety
/// `dc` must be a live DC and `font` a live font.
unsafe fn text_w(dc: HDC, font: HFONT, text: &str) -> i32 {
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    let mut rect = RECT::default();
    // SAFETY: the font and DC are the caller's, both live; `wide` and `rect`
    // outlive the call.
    unsafe {
        SelectObject(dc, HGDIOBJ(font.0));
        DrawTextW(
            dc,
            &mut wide,
            &mut rect,
            DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX,
        );
    }
    rect.right - rect.left
}

// --- card metrics ---------------------------------------------------------
//
// A card's height is the caller's, and the sum of what its body draws has to be
// exactly that height — a card whose contents overrun it draws its last row
// through its own bottom edge. These four functions are that sum written once,
// so the painter computing a height and the card drawing its contents are the
// same expression rather than two that happen to agree today.

/// The head a card leads with: the chip and the gap under it.
pub(crate) fn head_h(dpi: u32) -> i32 {
    scale(CHIP, dpi) + scale(S2, dpi)
}

/// One stat lane, gap included.
pub(crate) fn lane_h(dpi: u32) -> i32 {
    scale(TILE_H, dpi) + scale(S2, dpi)
}

/// One meter row, gap included.
pub(crate) fn meter_h(row_h: i32, dpi: u32) -> i32 {
    row_h + scale(TRACK_H, dpi) + scale(S2, dpi)
}

/// A card's outer height, from the height of what goes inside it.
pub(crate) fn card_h(dpi: u32, content: i32) -> i32 {
    scale(2 * CARD_PAD, dpi) + content
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
/// `icon` travels with them because two places draw a glyph now — the sidebar
/// and the Settings page's appearance row — and the sidebar sizes its own from
/// the same cached `design::icon_font`, so the two are the same handle and
/// never two sizes of the same picture in one window.
pub(crate) struct Fonts {
    pub body: HFONT,
    /// One step heavier, for values, so the numbers lead and the captions
    /// annotate.
    pub bold: HFONT,
    /// The page heading.
    pub title: HFONT,
    /// A section caption, between the body and the heading.
    pub caption: HFONT,
    /// The Stopwatch page's reading. Body-sized on every other page by
    /// construction — nothing but the clock reaches for it.
    pub clock: HFONT,
    /// The icon face, for the few glyphs drawn inside the content area.
    pub icon: HFONT,
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
    /// Not private: the history chart is a painter of its own rather than a
    /// component, because it fills a rectangle the frame sizes at the last
    /// moment — and what it borrows from here is the frame's DC, faces, colours
    /// and row metrics, which are the same four things every component uses.
    pub(crate) dc: HDC,
    pub(crate) fonts: &'a Fonts,
    pub(crate) pal: &'a Palette,
    /// Content column. A card moves this inward for its own contents, so it is
    /// where the *next* component draws rather than where the page began.
    pub(crate) x0: i32,
    pub(crate) x1: i32,
    /// Where the next component starts.
    y: i32,
    pub(crate) row_h: i32,
    /// A row's text sits a little below its band's top, which is what makes a
    /// label and its value look level.
    pub(crate) nudge: i32,
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

    /// A faint rule on a band of its own, dividing the rows above it from the
    /// rows below.
    ///
    /// The counterpart of `section` for a group whose heading is already
    /// painted: a `section` caption is a component's own text, drawn at the
    /// content edge, while the Settings page's captions sit *behind* a control
    /// at that same edge and would be covered by it. This draws the one part of
    /// a section that carries no text, and claims a band so that the rule has
    /// nothing to run through.
    pub(crate) fn rule(&mut self) {
        // SAFETY: a live DC, and `hairline` documents its own contract.
        unsafe {
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
        self.row_aligned(label, value, 0);
    }

    /// A row whose text drops by `dy` before it is drawn.
    ///
    /// For the one place on the page where a caption sits *beside* a native
    /// control instead of above one. A control centres its own text in its
    /// rectangle; a row draws from the top of its band; the two then disagree
    /// by a few pixels — enough that the caption reads as a heading for the row
    /// above it. `dy` is measured against the real control rather than derived,
    /// because how a control centres its text is the control's own business.
    pub(crate) fn row_aligned(&mut self, label: &str, value: &str, dy: i32) {
        // SAFETY: a live DC and fonts owned by the caller's frame.
        unsafe {
            SetTextColor(self.dc, self.text_colour());
            draw(
                self.dc,
                self.fonts.body,
                label,
                self.label_x(),
                self.y + self.nudge + dy,
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
                self.y + self.nudge + dy,
                self.x1,
                DT_RIGHT | DT_END_ELLIPSIS,
            );
        }
        self.y += self.row_h;
    }

    /// A row whose caption leads with a glyph.
    ///
    /// The one row in the window that carries an icon outside the sidebar, and
    /// it is not a whole new component: a glyph is a character in the icon face
    /// at `x0`, and the caption follows it at the icon column the sidebar
    /// already reserves, so the two line up with every other glyph in the
    /// window. `dy` is the field-row drop, because the control beside it is a
    /// native one and centres its own text.
    pub(crate) fn row_glyph(&mut self, cp: u16, label: &str, dy: i32) {
        // SAFETY: a live DC and faces owned by the caller's frame; `glyph` and
        // `draw` each document their own contract.
        unsafe {
            SetTextColor(self.dc, self.text_colour());
            glyph(self.dc, self.fonts.icon, cp, self.label_x(), self.y + self.nudge + dy);
            draw(
                self.dc,
                self.fonts.body,
                label,
                self.label_x() + ICON_COL,
                self.y + self.nudge + dy,
                self.x1,
                DT_LEFT,
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

    /// A selected row's fill, on the band the cursor is on.
    ///
    /// Drawn *before* the row rather than after it, so the row's own text sits
    /// on top: a pill painted afterwards would cover the label it belongs to.
    /// It claims no space — it is the same band, filled — so a selection
    /// appearing cannot move any row.
    pub(crate) fn pill(&mut self) {
        // SAFETY: a live DC; `rounded_fill` documents its own contract.
        unsafe {
            rounded_fill(
                self.dc,
                RECT {
                    left: self.x0,
                    top: self.y,
                    right: self.x1,
                    bottom: self.y + self.row_h,
                },
                RADIUS,
                self.pal.selected,
            );
        }
    }

    /// A row with a progress track drawn in the band under it.
    ///
    /// The track is inside the row's own band rather than on one of its own, so
    /// a bar that appears the moment a run starts cannot push the rows beneath
    /// it down — the same reason the group headings ride on a row. The band is
    /// 30 pixels and single-line text is about 17 of them, so the last few are
    /// free.
    pub(crate) fn progress(&mut self, label: &str, value: &str, percent: u32) {
        let top = self.y;
        self.row(label, value);

        let track = RECT {
            left: self.x0,
            // Pinned to the foot of the band, not to a fixed offset: the row
            // height is the caller's and this has to stay under the text at
            // every scale.
            top: top + self.row_h - S1,
            right: self.x1,
            bottom: top + self.row_h,
        };
        let filled = ((self.x1 - self.x0) * percent.min(100) as i32) / 100;
        // SAFETY: a live DC; `rounded_fill` documents its own contract.
        unsafe {
            rounded_fill(self.dc, track, 2, self.pal.border);
            if filled > 0 {
                rounded_fill(
                    self.dc,
                    RECT {
                        right: track.left + filled,
                        ..track
                    },
                    2,
                    self.pal.accent,
                );
            }
        }
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

    // --- the card components -------------------------------------------------

    /// A muted line under the page title saying what the page is for.
    ///
    /// One band, like a row, so the title block is a fixed height whether or not
    /// a page has a subtitle to offer.
    pub(crate) fn subtitle(&mut self, text: &str) {
        // SAFETY: a live DC and faces owned by the caller's frame.
        unsafe {
            SetTextColor(self.dc, self.pal.muted);
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

    /// A rounded pill at the right edge of the current band: the date, on the
    /// Overview page.
    ///
    /// Claims **no** space. It is drawn on the band the heading already owns —
    /// the heading is a large face with air under it and the pill is a small
    /// object, so the two share a band and the pill reads as belonging to the
    /// title rather than as a row of its own.
    pub(crate) fn date_pill(&mut self, dpi: u32, text: &str) {
        let pad = scale(S3, dpi);
        let glyph_w = scale(ICON_COL, dpi);
        let h = scale(CHIP, dpi);
        let top = self.y + scale(S1, dpi);
        // Measured rather than estimated: a date is ten characters of digits and
        // spaces whose width depends on the face, and a pill cut to a guessed
        // width clips the year it exists to show.
        // SAFETY: a live DC and the frame's body face.
        let width = unsafe { text_w(self.dc, self.fonts.body, text) } + pad * 2 + glyph_w;
        let right = self.x1;
        let left = (right - width).max(self.x0);
        let text_top = top + (h - self.row_h) / 2 + self.nudge;
        // SAFETY: a live DC and faces owned by the caller's frame; `rounded_fill`
        // and `glyph` each document their own contract.
        unsafe {
            rounded_fill(
                self.dc,
                RECT {
                    left,
                    top,
                    right,
                    bottom: top + h,
                },
                h / 2,
                self.pal.card,
            );
            SetTextColor(self.dc, self.pal.muted);
            glyph(self.dc, self.fonts.icon, ICON_CALENDAR, left + pad, text_top);
            draw(
                self.dc,
                self.fonts.body,
                text,
                left + pad + glyph_w,
                text_top,
                right - pad,
                DT_LEFT,
            );
        }
    }

    /// A card: a raised plate the contents are drawn on, and a gap after it.
    ///
    /// The contents move `x0`/`x1` inward for the duration of `body` and the
    /// cursor starts past the card's own padding, so everything drawn inside —
    /// a row, a meter, a chart — is inset without knowing a card exists. Both
    /// are restored afterwards, because the next card is full width again.
    ///
    /// `height` is the card's **outer** height, and the body has to draw exactly
    /// that much minus the padding. `card_h` computes it from the contents so
    /// the painter never adds those numbers up by hand.
    ///
    /// The cursor lands on the card's bottom edge plus the lane gap, whatever
    /// the body left behind: a body that drew short would otherwise pull the
    /// next card up into this one's plate.
    pub(crate) fn card<F>(&mut self, dpi: u32, height: i32, body: F)
    where
        F: FnOnce(&mut Self),
    {
        let top = self.y;
        // SAFETY: a live DC; `rounded_fill` documents its own contract.
        unsafe {
            rounded_fill(
                self.dc,
                RECT {
                    left: self.x0,
                    top,
                    right: self.x1,
                    bottom: top + height,
                },
                scale(CARD_RADIUS, dpi),
                self.pal.card,
            );
        }
        let (outer_x0, outer_x1) = (self.x0, self.x1);
        let pad = scale(CARD_PAD, dpi);
        self.x0 += pad;
        self.x1 -= pad;
        self.y = top + pad;
        body(self);
        self.x0 = outer_x0;
        self.x1 = outer_x1;
        self.y = top + height + scale(LANE_GAP, dpi);
    }

    /// A card's head: a tinted chip, its title, and a muted note at the right.
    ///
    /// The chip is the rounded-square kind, which is what tells a card's heading
    /// apart from a stat tile's circular one in the same window.
    pub(crate) fn card_head(&mut self, dpi: u32, cp: u16, tint: COLORREF, title: &str, right: &str) {
        let size = scale(CHIP, dpi);
        let top = self.y;
        let text_top = top + (size - self.row_h) / 2 + self.nudge;
        // SAFETY: a live DC and faces owned by the caller's frame; `chip` and
        // `draw` each document their own contract.
        unsafe {
            chip(
                self.dc,
                self.fonts.icon,
                cp,
                RECT {
                    left: self.x0,
                    top,
                    right: self.x0 + size,
                    bottom: top + size,
                },
                tint,
                self.pal.card,
                false,
                dpi,
            );
            SetTextColor(self.dc, self.text_colour());
            draw(
                self.dc,
                self.fonts.bold,
                title,
                self.x0 + size + scale(S2, dpi),
                text_top,
                self.x1,
                DT_LEFT,
            );
            SetTextColor(self.dc, self.pal.muted);
            draw(
                self.dc,
                self.fonts.caption,
                right,
                self.x0,
                text_top,
                self.x1,
                DT_RIGHT | DT_END_ELLIPSIS,
            );
        }
        self.y = top + head_h(dpi);
    }

    /// Three readings side by side, each an icon chip with a value and a caption.
    ///
    /// A lane rather than three calls because the three have to be the same
    /// width: tiles sized to their own text would make a row of three ragged
    /// columns, and the eye reads a row of readings as a row only when they line
    /// up. The tint travels with each tile — three identical chips is a list of
    /// bullets, not three readings.
    ///
    /// Items are `(tint, glyph, value, caption)`.
    pub(crate) fn stat_lane(&mut self, dpi: u32, items: [(COLORREF, u16, &str, &str); 3]) {
        let gap = scale(LANE_GAP, dpi) / 2;
        let tile_h = scale(TILE_H, dpi);
        let chip_size = scale(CHIP, dpi);
        let top = self.y;
        let span = (self.x1 - self.x0 - gap * 2) / 3;
        // Two lines in a tile shorter than two rows, so the step is two thirds
        // of a band and the pair is centred against the chip.
        let step = self.row_h * 2 / 3;
        let text_top = top + (tile_h - step * 2) / 2;
        let chip_top = top + (tile_h - chip_size) / 2;
        for (index, (tint, cp, value, caption)) in items.into_iter().enumerate() {
            let left = self.x0 + (span + gap) * index as i32;
            // The last tile is pinned to the right edge rather than to
            // `left + span`: the integer division above leaves a few pixels over
            // and a lane that stops short of its own card looks like a mistake.
            let right = if index == 2 { self.x1 } else { left + span };
            let text_x = left + chip_size + scale(S2, dpi);
            // SAFETY: a live DC and faces owned by the caller's frame; `chip`
            // and `draw` each document their own contract.
            unsafe {
                chip(
                    self.dc,
                    self.fonts.icon,
                    cp,
                    RECT {
                        left,
                        top: chip_top,
                        right: left + chip_size,
                        bottom: chip_top + chip_size,
                    },
                    tint,
                    self.pal.card,
                    true,
                    dpi,
                );
                SetTextColor(self.dc, self.text_colour());
                draw(
                    self.dc,
                    self.fonts.bold,
                    value,
                    text_x,
                    text_top,
                    right,
                    DT_LEFT | DT_END_ELLIPSIS,
                );
                SetTextColor(self.dc, self.pal.muted);
                draw(
                    self.dc,
                    self.fonts.caption,
                    caption,
                    text_x,
                    text_top + step,
                    right,
                    DT_LEFT | DT_END_ELLIPSIS,
                );
            }
        }
        self.y = top + lane_h(dpi);
    }

    /// A reading as a proportion: its caption and value on a band, and a track
    /// under them filled to `pct` of the width.
    ///
    /// `pct` is a fraction, 0 to 1. The track runs the whole content width
    /// rather than a fixed share of it, so two meters in one card are read
    /// against the same scale — a bar that stopped two thirds of the way across
    /// would say "two thirds" twice.
    pub(crate) fn meter(&mut self, dpi: u32, label: &str, value: &str, pct: f32, tint: COLORREF) {
        let top = self.y;
        // SAFETY: a live DC and faces owned by the caller's frame.
        unsafe {
            SetTextColor(self.dc, self.text_colour());
            draw(
                self.dc,
                self.fonts.body,
                label,
                self.x0,
                top + self.nudge,
                self.x1,
                DT_LEFT,
            );
            draw(
                self.dc,
                self.fonts.bold,
                value,
                self.x0,
                top + self.nudge,
                self.x1,
                DT_RIGHT | DT_END_ELLIPSIS,
            );
        }
        let track_h = scale(TRACK_H, dpi);
        let track = RECT {
            left: self.x0,
            top: top + self.row_h,
            right: self.x1,
            bottom: top + self.row_h + track_h,
        };
        let filled = ((track.right - track.left) as f32 * pct.clamp(0.0, 1.0)) as i32;
        // SAFETY: a live DC; `rounded_fill` documents its own contract.
        unsafe {
            // A capsule, so the bar's ends match the chip it sits under.
            rounded_fill(self.dc, track, track_h / 2, self.pal.border);
            if filled > 0 {
                rounded_fill(
                    self.dc,
                    RECT {
                        right: track.left + filled,
                        ..track
                    },
                    track_h / 2,
                    tint,
                );
            }
        }
        self.y = top + meter_h(self.row_h, dpi);
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
            clock: HFONT::default(),
            icon: HFONT::default(),
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

        // The track is drawn *inside* the band the row already claimed, which
        // is the only reason a bar can appear mid-run without moving the page.
        let before_bar = c.y();
        c.progress("Progress", "42%", 42);
        assert_eq!(c.y() - before_bar, 30, "a progress row is one band, track included");

        let before_note = c.y();
        c.note("saved", pal.text);
        assert_eq!(c.y() - before_note, 30);

        // The one component that must *not* move it: the empty state stands in
        // for rows that are absent, so it takes its own gap and one band.
        let before_empty = c.y();
        c.empty("No usage recorded yet.");
        assert_eq!(c.y() - before_empty, 30 + 24);
    }

    /// The cards, and the one property that makes them safe to lay out: a card
    /// claims exactly the height it was given plus the lane gap, and restores
    /// the content column, whatever its body did.
    ///
    /// The height is the bug this guards against. A card's body and its height
    /// are computed in two places — the painter's sum and the drawing calls —
    /// and when they disagree the contents run out through the plate's bottom
    /// edge, which no test of the cursor alone would catch. Driving a real body
    /// that draws the same sum is what makes them one expression.
    #[test]
    fn a_card_claims_its_height_restores_the_column_and_insets_its_body() {
        let fonts = Fonts {
            body: HFONT::default(),
            bold: HFONT::default(),
            title: HFONT::default(),
            caption: HFONT::default(),
            clock: HFONT::default(),
            icon: HFONT::default(),
        };
        let pal = crate::ui::design::palette(&crate::config::Config::default());
        // 96 DPI, where every scaled constant is the constant.
        const DPI: u32 = 96;
        let mut c = Canvas::new(HDC::default(), &fonts, &pal, 0, 200, 30, 6, 100);

        let start = c.y();
        let mut inner_seen = (0, 0);
        let mut body_top = 0;
        let outer = card_h(DPI, head_h(DPI) + 2 * meter_h(30, DPI));
        c.card(DPI, outer, |c| {
            inner_seen = (c.x0, c.x1);
            body_top = c.y();
            c.card_head(DPI, 0xE9FA, pal.tile_blue, "Traffic", "Live");
            c.meter(DPI, "CPU", "10%", 0.1, pal.tile_blue);
            c.meter(DPI, "RAM", "40%", 0.4, pal.tile_green);
        });
        assert_eq!(
            c.y() - start,
            outer + LANE_GAP,
            "a card claims its own height plus the lane gap"
        );
        assert_eq!(
            (c.x0, c.x1),
            (0, 200),
            "the content column comes back out with the card"
        );
        assert_eq!(
            inner_seen,
            (CARD_PAD, 200 - CARD_PAD),
            "the body draws inside the padding"
        );
        assert_eq!(
            body_top,
            start + CARD_PAD,
            "and starts past the card's own top padding"
        );

        // The sub-components: a head is a chip and a gap, a lane is a tile and a
        // gap, a meter is a row plus its track and a gap. Pinned because the
        // painter sums these to size a card, so a change here silently changes
        // every card's height.
        assert_eq!(head_h(DPI), CHIP + S2);
        assert_eq!(lane_h(DPI), TILE_H + S2);
        assert_eq!(meter_h(30, DPI), 30 + TRACK_H + S2);

        // The pill rides a band without claiming one, which is the only reason
        // the date can sit on the title's own line.
        let before_pill = c.y();
        c.date_pill(DPI, "18 Sep 2026");
        assert_eq!(c.y(), before_pill, "the date pill must not claim a band");

        let before_sub = c.y();
        c.subtitle("Live traffic and load at a glance");
        assert_eq!(c.y() - before_sub, 30);
    }
}
